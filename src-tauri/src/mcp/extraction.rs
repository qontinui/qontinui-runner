//! Web extraction handlers for MCP API
//!
//! Provides handlers for web content extraction.

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use super::types::{api_error, ApiResponse, ApiState};
use crate::executor::with_default_bridge;

/// Request to start vision extraction
#[derive(Debug, Deserialize)]
pub struct StartVisionExtractionRequest {
    /// Base64-encoded screenshot or file path
    pub screenshot: String,
    /// Techniques to run: ["edge", "sam3", "ocr"]
    #[serde(default = "default_vision_techniques")]
    pub techniques: Vec<String>,
    /// Edge detection: Canny low threshold
    #[serde(default = "default_canny_low")]
    pub canny_low: i32,
    /// Edge detection: Canny high threshold
    #[serde(default = "default_canny_high")]
    pub canny_high: i32,
    /// Edge detection: minimum contour area
    #[serde(default = "default_min_contour_area")]
    pub min_contour_area: i32,
    /// SAM3: points per side
    #[serde(default = "default_points_per_side")]
    pub points_per_side: i32,
    /// SAM3: predicted IoU threshold
    #[serde(default = "default_pred_iou_thresh")]
    pub pred_iou_thresh: f64,
    /// SAM3: stability score threshold
    #[serde(default = "default_stability_score_thresh")]
    pub stability_score_thresh: f64,
    /// OCR: engine ("easyocr" or "tesseract")
    #[serde(default = "default_ocr_engine")]
    pub ocr_engine: String,
    /// OCR: languages
    #[serde(default = "default_ocr_languages")]
    pub ocr_languages: Vec<String>,
    /// OCR: confidence threshold
    #[serde(default = "default_ocr_confidence")]
    pub ocr_confidence_threshold: f64,
    /// Fusion: IoU threshold for deduplication
    #[serde(default = "default_iou_threshold")]
    pub iou_threshold: f64,
}

fn default_vision_techniques() -> Vec<String> {
    vec!["edge".to_string(), "ocr".to_string()]
}

fn default_canny_low() -> i32 {
    50
}

fn default_canny_high() -> i32 {
    150
}

fn default_min_contour_area() -> i32 {
    100
}

fn default_points_per_side() -> i32 {
    32
}

fn default_pred_iou_thresh() -> f64 {
    0.88
}

fn default_stability_score_thresh() -> f64 {
    0.95
}

fn default_ocr_engine() -> String {
    "easyocr".to_string()
}

fn default_ocr_languages() -> Vec<String> {
    vec!["en".to_string()]
}

fn default_ocr_confidence() -> f64 {
    0.6
}

fn default_iou_threshold() -> f64 {
    0.5
}

/// Request to start web extraction
#[derive(Debug, Deserialize)]
pub struct StartExtractionRequest {
    /// URLs to extract from
    pub urls: Vec<String>,
    /// Viewport sizes as [width, height] pairs
    #[serde(default)]
    pub viewports: Vec<(u32, u32)>,
    /// Whether to capture hover states
    #[serde(default = "default_true")]
    pub capture_hover_states: bool,
    /// Whether to capture focus states
    #[serde(default = "default_true")]
    pub capture_focus_states: bool,
    /// Maximum crawl depth
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum pages to crawl
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    /// Backend session ID to update with progress
    #[serde(default)]
    pub session_id: Option<String>,
    /// Backend API URL for progress updates
    #[serde(default)]
    pub backend_url: Option<String>,
    /// Auth token for backend API calls
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_max_depth() -> u32 {
    5
}

fn default_max_pages() -> u32 {
    100
}

/// Response from extraction status endpoint
#[derive(Debug, Serialize)]
pub struct ExtractionStatusResponse {
    pub is_running: bool,
    pub extraction_id: Option<String>,
    pub stats: Option<ExtractionStats>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExtractionStats {
    pub states_found: u32,
    pub transitions_found: u32,
    pub warnings: u32,
    pub errors: u32,
}

/// Start web extraction
pub async fn start_web_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting web extraction for {} URLs",
        request.urls.len()
    );

    // Build extraction params
    let params = serde_json::json!({
        "urls": request.urls,
        "viewports": request.viewports,
        "capture_hover_states": request.capture_hover_states,
        "capture_focus_states": request.capture_focus_states,
        "max_depth": request.max_depth,
        "max_pages": request.max_pages,
        "session_id": request.session_id,
        "backend_url": request.backend_url,
        "auth_token": request.auth_token,
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("start_web_extraction", Some(params))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Web extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "Web extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Start vision extraction
pub async fn start_vision_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartVisionExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Starting vision extraction");

    // Build extraction params
    let params = serde_json::json!({
        "config": {
            "screenshot": request.screenshot,
            "techniques": request.techniques,
            "canny_low": request.canny_low,
            "canny_high": request.canny_high,
            "min_contour_area": request.min_contour_area,
            "points_per_side": request.points_per_side,
            "pred_iou_thresh": request.pred_iou_thresh,
            "stability_score_thresh": request.stability_score_thresh,
            "ocr_engine": request.ocr_engine,
            "ocr_languages": request.ocr_languages,
            "ocr_confidence_threshold": request.ocr_confidence_threshold,
            "iou_threshold": request.iou_threshold,
        }
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("run_vision_extraction", Some(params))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Vision extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "Vision extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start vision extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop web extraction
pub async fn stop_web_extraction(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping web extraction");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_web_extraction", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Web extraction stopped");
            Ok(Json(ApiResponse::success(
                "Web extraction stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get extraction status
pub async fn get_extraction_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ExtractionStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("get_extraction_status", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            // Note: send_command doesn't return data, so we return a default status
            // TODO: Implement proper extraction status tracking in app state
            Ok(Json(ApiResponse::success(ExtractionStatusResponse {
                is_running: false,
                extraction_id: None,
                stats: None,
            })))
        }
        Err(e) => {
            error!("MCP API: Failed to get extraction status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get extraction screenshot
///
/// Serves a screenshot image from a web extraction session.
/// The screenshot is stored locally on the runner machine.
pub async fn get_extraction_screenshot(
    axum::extract::Path((extraction_id, screenshot_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    // Build path to screenshot file
    let home_dir = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let screenshot_path = home_dir
        .join(".qontinui")
        .join("extractions")
        .join(&extraction_id)
        .join("screenshots")
        .join(format!("{}.png", screenshot_id));

    // Try to read the file
    match tokio::fs::read(&screenshot_path).await {
        Ok(data) => {
            // Return the image with appropriate headers
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "image/png")],
                Body::from(data),
            )
                .into_response()
        }
        Err(e) => {
            error!(
                "Failed to read screenshot {}/{}: {}",
                extraction_id, screenshot_id, e
            );
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(format!("Screenshot not found: {}", e)),
            )
                .into_response()
        }
    }
}
