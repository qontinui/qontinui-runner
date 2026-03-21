//! Extraction handlers for MCP API
//!
//! Provides HTTP handlers for web extraction, vision extraction,
//! UI-TARS extraction, and pattern matching operations.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Web Extraction Types
// ============================================================================

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
    /// Enable comprehensive extraction pipeline (captures ALL visible elements)
    #[serde(default)]
    pub use_comprehensive_extraction: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtractionStats {
    pub states_found: u32,
    pub transitions_found: u32,
    pub pages_extracted: u32,
    pub warnings: u32,
    pub errors: u32,
}

/// Tracks the current state of web extraction
/// Thread-safe wrapper for extraction status tracking
#[derive(Debug, Default)]
pub struct ExtractionState {
    inner: std::sync::Mutex<ExtractionStateInner>,
}

#[derive(Debug, Default)]
struct ExtractionStateInner {
    is_running: bool,
    extraction_id: Option<String>,
    stats: ExtractionStats,
}

impl ExtractionState {
    /// Create a new extraction state tracker
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(ExtractionStateInner::default()),
        }
    }

    /// Mark extraction as started with the given ID
    pub fn start(&self, extraction_id: Option<String>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = true;
        inner.extraction_id = extraction_id;
        inner.stats = ExtractionStats::default();
    }

    /// Mark extraction as stopped
    pub fn stop(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = false;
    }

    /// Mark extraction as complete with final stats
    pub fn complete(&self, stats: ExtractionStats) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = false;
        inner.stats = stats;
    }

    /// Update the extraction stats
    pub fn update_stats(&self, stats: ExtractionStats) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.stats = stats;
    }

    /// Get the current extraction status
    pub fn get_status(&self) -> ExtractionStatusResponse {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ExtractionStatusResponse {
            is_running: inner.is_running,
            extraction_id: inner.extraction_id.clone(),
            stats: if inner.is_running
                || inner.stats.states_found > 0
                || inner.stats.pages_extracted > 0
            {
                Some(inner.stats.clone())
            } else {
                None
            },
        }
    }
}

// =============================================================================
// UI-TARS Extraction Types
// =============================================================================

/// Request to start UI-TARS extraction
#[derive(Debug, Deserialize)]
pub struct StartUITarsExtractionRequest {
    /// Target type: "web" or "desktop"
    #[serde(default = "default_desktop")]
    pub target_type: String,
    /// Target URL (for web) or application name (for desktop)
    pub target: String,
    /// Exploration goal (what to discover)
    #[serde(default = "default_uitars_goal")]
    pub goal: String,
    /// Provider: "local_transformers", "local_vllm", or "cloud"
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model size: "2B", "7B", or "72B"
    #[serde(default = "default_model_size")]
    pub model_size: String,
    /// Quantization: "none", "int8", or "int4"
    #[serde(default = "default_quantization")]
    pub quantization: String,
    /// Maximum exploration steps
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Timeout in seconds
    #[serde(default = "default_uitars_timeout")]
    pub timeout_seconds: u32,
    /// Whether to save screenshots
    #[serde(default = "default_true")]
    pub save_screenshots: bool,
    /// HuggingFace endpoint (for cloud provider)
    #[serde(default)]
    pub huggingface_endpoint: Option<String>,
    /// HuggingFace API token (for cloud provider)
    #[serde(default)]
    pub huggingface_api_token: Option<String>,
    /// vLLM server URL (for local_vllm provider)
    #[serde(default)]
    pub vllm_server_url: Option<String>,
    /// Monitor index for desktop extraction
    #[serde(default)]
    pub monitor_index: u32,
}

fn default_desktop() -> String {
    "desktop".to_string()
}

fn default_uitars_goal() -> String {
    "Explore the application and discover all clickable UI elements including buttons, links, menu items, and interactive controls. Identify distinct application states and the actions that transition between them.".to_string()
}

fn default_provider() -> String {
    "local_transformers".to_string()
}

fn default_model_size() -> String {
    "2B".to_string()
}

fn default_quantization() -> String {
    "int4".to_string()
}

fn default_max_steps() -> u32 {
    50
}

/// Default timeout for UI-TARS extraction.
/// Returns 0 to indicate no timeout (run until completion).
fn default_uitars_timeout() -> u32 {
    0 // No timeout - run until completion
}

/// Response from UI-TARS extraction status endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsExtractionStatusResponse {
    pub status: String,
    pub current_step: u32,
    pub max_steps: u32,
    pub elapsed_seconds: f64,
    pub last_thought: Option<String>,
    pub last_action: Option<String>,
    pub states_discovered: u32,
    pub transitions_discovered: u32,
    pub error_message: Option<String>,
    pub uitars_available: bool,
}

/// Response from UI-TARS extraction results endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsExtractionResultsResponse {
    pub states: Vec<UITarsDiscoveredState>,
    pub transitions: Vec<UITarsDiscoveredTransition>,
    pub total_steps: u32,
    pub total_screenshots: u32,
    pub exploration_time_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsDiscoveredState {
    pub id: String,
    pub name: String,
    pub description: String,
    pub screenshot_path: String,
    pub elements: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsDiscoveredTransition {
    pub id: String,
    pub from_state_id: String,
    pub to_state_id: String,
    pub action_type: String,
    pub action_description: String,
    pub coordinates: Option<(i32, i32)>,
}

// ============================================================================
// Vision Extraction Types
// ============================================================================

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

/// Request for vision extraction
#[derive(Debug, Deserialize)]
pub struct VisionExtractionRequest {
    /// Base64-encoded screenshot image
    pub screenshot: String,
    /// Techniques to run: "edge", "sam3", "ocr"
    #[serde(default = "default_vision_techniques")]
    pub techniques: Vec<String>,
    /// Edge detection: lower Canny threshold
    #[serde(default = "default_canny_low")]
    pub canny_low: i32,
    /// Edge detection: upper Canny threshold
    #[serde(default = "default_canny_high")]
    pub canny_high: i32,
    /// Edge detection: minimum contour area
    #[serde(default = "default_min_contour_area")]
    pub min_contour_area: i32,
    /// SAM3: points per side for mask generation
    #[serde(default = "default_points_per_side")]
    pub points_per_side: i32,
    /// SAM3: predicted IoU threshold
    #[serde(default = "default_pred_iou_thresh")]
    pub pred_iou_thresh: f64,
    /// SAM3: stability score threshold
    #[serde(default = "default_stability_score_thresh")]
    pub stability_score_thresh: f64,
    /// OCR: engine to use ("easyocr" or "pytesseract")
    #[serde(default = "default_ocr_engine")]
    pub ocr_engine: String,
    /// OCR: languages to detect
    #[serde(default = "default_ocr_languages")]
    pub ocr_languages: Vec<String>,
    /// OCR: confidence threshold
    #[serde(default = "default_ocr_confidence")]
    pub ocr_confidence_threshold: f64,
    /// Fusion: IoU threshold for merging results
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

// ============================================================================
// Pattern Matching Types
// ============================================================================

/// Search region for pattern matching
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SearchRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Request for pattern matching
#[derive(Debug, Deserialize)]
pub struct PatternMatchRequest {
    /// Base64 encoded screenshot or file path
    pub screenshot: String,
    /// Base64 encoded template image or file path
    pub template: String,
    /// Minimum similarity threshold (0.0 to 1.0, default: 0.8)
    #[serde(default = "default_similarity")]
    pub similarity: f32,
    /// Optional search region
    #[serde(default)]
    pub search_region: Option<SearchRegion>,
    /// Maximum matches for find_all (default: 100)
    #[serde(default = "default_max_matches")]
    pub max_matches: Option<i32>,
}

fn default_similarity() -> f32 {
    0.8
}

fn default_max_matches() -> Option<i32> {
    Some(100)
}

/// Match result from pattern matching
#[derive(Debug, Serialize)]
pub struct PatternMatch {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub similarity: f32,
    pub center_x: i32,
    pub center_y: i32,
}

/// Response from pattern matching
#[derive(Debug, Serialize)]
pub struct PatternMatchResponse {
    pub success: bool,
    pub matches: Vec<PatternMatch>,
    pub search_time_ms: f32,
    pub screenshot_width: i32,
    pub screenshot_height: i32,
    pub template_width: i32,
    pub template_height: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// Start web extraction
#[tracing::instrument(
    name = "api.request.start_web_extraction",
    skip(state, request),
    fields(
        endpoint = "/extraction/start",
        method = "POST",
        url_count = %request.urls.len()
    )
)]
pub async fn start_web_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting web extraction for {} URLs",
        request.urls.len()
    );

    // Generate an extraction ID from session_id or create a new one
    let extraction_id = request
        .session_id
        .clone()
        .unwrap_or_else(|| format!("extraction_{}", chrono::Utc::now().timestamp_millis()));

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
        "use_comprehensive_extraction": request.use_comprehensive_extraction,
    });

    let app_state = state.app_state.clone();
    let extraction_state = state.extraction_state.clone();
    let extraction_id_for_state = extraction_id.clone();

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
            // Mark extraction as running
            extraction_state.start(Some(extraction_id_for_state.clone()));
            info!(
                "MCP API: Web extraction started with ID: {}",
                extraction_id_for_state
            );
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "extraction_id": extraction_id_for_state,
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
    let extraction_state = state.extraction_state.clone();

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
            // Mark extraction as stopped
            extraction_state.stop();
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
    // Return the tracked extraction state
    let status = state.extraction_state.get_status();
    debug!(
        "MCP API: Extraction status - is_running: {}, extraction_id: {:?}",
        status.is_running, status.extraction_id
    );
    Ok(Json(ApiResponse::success(status)))
}

/// Update extraction stats
///
/// Called by the Python extraction process to report progress.
pub async fn update_extraction_stats(
    State(state): State<Arc<ApiState>>,
    Json(stats): Json<ExtractionStats>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    debug!(
        "MCP API: Updating extraction stats - states: {}, transitions: {}, pages: {}, errors: {}",
        stats.states_found, stats.transitions_found, stats.pages_extracted, stats.errors
    );
    state.extraction_state.update_stats(stats);
    Ok(Json(ApiResponse::success("Stats updated".to_string())))
}

/// Mark extraction as complete
///
/// Called by the Python extraction process when extraction finishes.
pub async fn complete_extraction(
    State(state): State<Arc<ApiState>>,
    Json(stats): Json<ExtractionStats>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Extraction complete - states: {}, transitions: {}, pages: {}, errors: {}",
        stats.states_found, stats.transitions_found, stats.pages_extracted, stats.errors
    );
    state.extraction_state.complete(stats);
    Ok(Json(ApiResponse::success(
        "Extraction completed".to_string(),
    )))
}

/// Get extraction screenshot
///
/// Serves a screenshot image from a web extraction session.
/// The screenshot is stored locally on the runner machine.
pub async fn get_extraction_screenshot(
    axum::extract::Path((extraction_id, screenshot_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use axum::body::Body;
    use axum::http::header;

    // Build path to screenshot file
    let home_dir = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let screenshot_path = home_dir
        .join(".qontinui")
        .join("extraction")
        .join(&extraction_id)
        .join("screenshots")
        .join(format!("{}.png", screenshot_id));

    info!(
        "MCP API: Serving extraction screenshot: {} from {:?}",
        screenshot_id, screenshot_path
    );

    // Check if file exists and read it
    match tokio::fs::read(&screenshot_path).await {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            Body::from(data),
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!("Screenshot not found: {:?}", screenshot_path);
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from(r#"{"error": "Screenshot not found"}"#),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to read screenshot file: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from(format!(
                    r#"{{"error": "Failed to read screenshot: {}"}}"#,
                    e
                )),
            )
                .into_response()
        }
    }
}

// ============================================================================
// UI-TARS Extraction Endpoints
// ============================================================================

/// Start UI-TARS extraction
pub async fn start_uitars_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartUITarsExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting UI-TARS extraction for target: {}",
        request.target
    );

    // Build extraction params
    let params = serde_json::json!({
        "target_type": request.target_type,
        "target": request.target,
        "goal": request.goal,
        "provider": request.provider,
        "model_size": request.model_size,
        "quantization": request.quantization,
        "max_steps": request.max_steps,
        "timeout_seconds": request.timeout_seconds,
        "save_screenshots": request.save_screenshots,
        "huggingface_endpoint": request.huggingface_endpoint,
        "huggingface_api_token": request.huggingface_api_token,
        "vllm_server_url": request.vllm_server_url,
        "monitor_index": request.monitor_index,
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("start_uitars_extraction", Some(params))
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
            info!("MCP API: UI-TARS extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "UI-TARS extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start UI-TARS extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop UI-TARS extraction
pub async fn stop_uitars_extraction(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI-TARS extraction");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_uitars_extraction", None)
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
            info!("MCP API: UI-TARS extraction stopped");
            Ok(Json(ApiResponse::success(
                "UI-TARS extraction stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI-TARS extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI-TARS extraction status
pub async fn get_uitars_extraction_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            // 60 second timeout for status check
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait("get_uitars_extraction_status", None, timeout)
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
        Ok(response) => {
            // Return the actual status from Python
            if response.success {
                Ok(Json(ApiResponse::success(response.data.unwrap_or(
                    serde_json::json!({
                        "status": "idle",
                        "current_step": 0,
                        "max_steps": 0,
                        "elapsed_seconds": 0.0,
                        "states_discovered": 0,
                        "transitions_discovered": 0,
                        "uitars_available": false
                    }),
                ))))
            } else {
                error!(
                    "MCP API: UI-TARS extraction status command failed: {:?}",
                    response.error
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(
                        response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    )),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI-TARS extraction status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI-TARS extraction results
pub async fn get_uitars_extraction_results(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            // 5 minute timeout for results fetch (may involve processing)
            let timeout = std::time::Duration::from_secs(300);
            bridge.send_command_and_wait("get_uitars_extraction_results", None, timeout)
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
        Ok(response) => {
            // Return the actual results from Python
            if response.success {
                Ok(Json(ApiResponse::success(response.data.unwrap_or(
                    serde_json::json!({
                        "states": [],
                        "transitions": [],
                        "total_steps": 0,
                        "total_screenshots": 0,
                        "exploration_time_seconds": 0.0
                    }),
                ))))
            } else {
                error!(
                    "MCP API: UI-TARS extraction results command failed: {:?}",
                    response.error
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(
                        response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    )),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI-TARS extraction results: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Vision Extraction Handler
// ============================================================================

/// Run vision extraction on a screenshot
///
/// This endpoint receives a base64-encoded screenshot and runs computer vision
/// algorithms (Edge Detection, SAM3 segmentation, OCR) on the user's machine.
pub async fn run_vision_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<VisionExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running vision extraction ({} bytes base64, techniques: {:?})",
        request.screenshot.len(),
        request.techniques
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
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
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (3 minute timeout for vision processing)
            let timeout_duration = std::time::Duration::from_secs(180);
            bridge.send_command_and_wait("run_vision_extraction", Some(params), timeout_duration)
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

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Vision extraction completed in {}ms",
                    elapsed.as_millis()
                );

                // Return the full response data from Python
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "processing_time_ms": elapsed.as_millis() as i64
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Vision extraction failed".to_string());
                error!("MCP API: Vision extraction failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "processing_time_ms": elapsed.as_millis() as i64
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run vision extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Pattern Matching Handlers
// ============================================================================

/// Find best match of template in screenshot
#[tracing::instrument(
    name = "api.request.pattern_find",
    skip(state, request),
    fields(
        endpoint = "/pattern/find",
        method = "POST",
        screenshot_size = %request.screenshot.len(),
        template_size = %request.template.len(),
        similarity = %request.similarity
    )
)]
pub async fn pattern_find(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PatternMatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Pattern find (screenshot: {} bytes, template: {} bytes, similarity: {})",
        request.screenshot.len(),
        request.template.len(),
        request.similarity
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot": request.screenshot,
        "template": request.template,
        "similarity": request.similarity,
        "search_region": request.search_region,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (30 second timeout)
            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("pattern_find", Some(params), timeout_duration)
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

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Pattern find completed in {}ms",
                    elapsed.as_millis()
                );

                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "matches": [],
                        "search_time_ms": elapsed.as_millis() as f32
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Pattern find failed".to_string());
                error!("MCP API: Pattern find failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "matches": [],
                    "search_time_ms": elapsed.as_millis() as f32
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run pattern find: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Find all matches of template in screenshot
#[tracing::instrument(
    name = "api.request.pattern_find_all",
    skip(state, request),
    fields(
        endpoint = "/pattern/find-all",
        method = "POST",
        screenshot_size = %request.screenshot.len(),
        template_size = %request.template.len(),
        similarity = %request.similarity,
        max_matches = ?request.max_matches
    )
)]
pub async fn pattern_find_all(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PatternMatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Pattern find all (screenshot: {} bytes, template: {} bytes, similarity: {}, max_matches: {:?})",
        request.screenshot.len(),
        request.template.len(),
        request.similarity,
        request.max_matches
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot": request.screenshot,
        "template": request.template,
        "similarity": request.similarity,
        "search_region": request.search_region,
        "max_matches": request.max_matches.unwrap_or(100),
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (30 second timeout)
            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("pattern_find_all", Some(params), timeout_duration)
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

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Pattern find all completed in {}ms",
                    elapsed.as_millis()
                );

                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "matches": [],
                        "search_time_ms": elapsed.as_millis() as f32
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Pattern find all failed".to_string());
                error!("MCP API: Pattern find all failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "matches": [],
                    "search_time_ms": elapsed.as_millis() as f32
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run pattern find all: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/extraction/start", post(start_web_extraction))
        .route("/extraction/vision", post(start_vision_extraction))
        .route("/extraction/stop", post(stop_web_extraction))
        .route("/extraction/status", get(get_extraction_status))
        .route("/extraction/stats", post(update_extraction_stats))
        .route("/extraction/complete", post(complete_extraction))
        .route(
            "/extraction/{extraction_id}/screenshot/{screenshot_id}",
            get(get_extraction_screenshot),
        )
        .route("/uitars-extraction/start", post(start_uitars_extraction))
        .route("/uitars-extraction/stop", post(stop_uitars_extraction))
        .route(
            "/uitars-extraction/status",
            get(get_uitars_extraction_status),
        )
        .route(
            "/uitars-extraction/results",
            get(get_uitars_extraction_results),
        )
        .route("/vision-extraction/extract", post(run_vision_extraction))
        .route("/pattern/find", post(pattern_find))
        .route("/pattern/find-all", post(pattern_find_all))
}
