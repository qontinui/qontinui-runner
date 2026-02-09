//! RAG handlers for MCP API
//!
//! Provides HTTP handlers for RAG configuration management:
//! import, list, status, delete, load, availability, and screenshot segmentation.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::commands::rag::send_embeddings_to_web;
use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::timeout_config::Timeouts;

// ============================================================================
// Types
// ============================================================================

/// Request to import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// The runner extracts what it needs (images, states, patterns) internally.
/// This eliminates the need for frontend transformation code.
#[derive(Debug, Deserialize)]
pub struct ImportRAGRequest {
    /// Full QontinuiConfig - the canonical format from TypeScript/Python
    pub config: QontinuiConfig,
    /// Optional project_id override (defaults to derived from metadata.name)
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Request to segment a screenshot using SAM3
#[derive(Debug, Deserialize)]
pub struct SegmentScreenshotRequest {
    /// Base64-encoded screenshot image (PNG or JPEG)
    pub screenshot_base64: String,
    /// Optional minimum segment area in pixels
    #[serde(default)]
    pub min_area: Option<i32>,
    /// Optional SAM model to use (e.g., "sam2_hiera_tiny")
    #[serde(default)]
    pub model: Option<String>,
}

/// Segment in the response
#[derive(Debug, Serialize)]
pub struct SegmentInfo {
    /// Unique segment ID
    pub id: String,
    /// Bounding box [x, y, width, height]
    pub bbox: Vec<i32>,
    /// Segment area in pixels
    pub area: i32,
    /// Base64-encoded cropped image of the segment
    pub image_base64: Option<String>,
}

/// Response from screenshot segmentation
#[derive(Debug, Serialize)]
pub struct SegmentScreenshotResponse {
    /// Whether segmentation was successful
    pub success: bool,
    /// List of detected segments
    pub segments: Vec<SegmentInfo>,
    /// Error message if failed
    pub error: Option<String>,
    /// Processing time in milliseconds
    pub processing_time_ms: Option<i64>,
}

// ============================================================================
// Handlers
// ============================================================================

/// Import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// Saves the complete config and extracts images for embedding generation.
pub async fn import_rag(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportRAGRequest>,
) -> Result<Json<ApiResponse<ImportResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Use provided project_id or derive from metadata.name
    let project_id = request
        .project_id
        .clone()
        .unwrap_or_else(|| request.config.project_id());

    let image_count = request.config.images.len();
    let state_image_count = request.config.state_image_count();
    let pattern_count = request.config.pattern_count();

    info!(
        "MCP API: Importing QontinuiConfig: project_id={}, name={}, images={}, states={}, stateImages={}, patterns={}",
        project_id,
        request.config.metadata.name,
        image_count,
        request.config.states.len(),
        state_image_count,
        pattern_count
    );

    // Save the full QontinuiConfig
    let storage = state.rag_state.storage.lock().await;
    let storage_path = storage
        .save_qontinui_config(&project_id, &request.config)
        .map_err(|e| {
            error!("Failed to save QontinuiConfig: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save config: {}", e))),
            )
        })?;

    // Extract and save images from config.images[]
    // Only save images that are referenced by patterns
    let referenced_ids = request.config.referenced_image_ids();
    let saved_image_count = storage
        .save_images_from_config(&project_id, &request.config.images, &referenced_ids)
        .map_err(|e| {
            error!("Failed to save images: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save images: {}", e))),
            )
        })?;

    let storage_path_str = storage_path.to_string_lossy().to_string();
    drop(storage);

    // Trigger embedding generation in background
    info!(
        "MCP API: Starting background embedding generation for project_id={}",
        project_id
    );
    let embedding_generator = state.rag_state.embedding_generator.lock().await;
    let mut progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    // Spawn background task to sync embeddings to web backend when complete
    let project_id_for_sync = project_id.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            match progress.status {
                crate::rag::EmbeddingStatus::Completed => {
                    info!(
                        "MCP API: Embedding generation completed for project_id={}, syncing to web backend",
                        project_id_for_sync
                    );
                    match send_embeddings_to_web(&project_id_for_sync).await {
                        Ok(()) => {
                            info!(
                                "MCP API: Successfully synced embeddings to web for project_id={}",
                                project_id_for_sync
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "MCP API: Failed to sync embeddings to web for project_id={}: {}",
                                project_id_for_sync,
                                e
                            );
                        }
                    }
                    break;
                }
                crate::rag::EmbeddingStatus::Failed(ref err) => {
                    tracing::warn!(
                        "MCP API: Embedding generation failed for project_id={}: {}",
                        project_id_for_sync,
                        err
                    );
                    break;
                }
                _ => {
                    // Continue polling for in-progress updates
                }
            }
        }
    });

    // Auto-add to ConfigStorage (database)
    let config_name = request.config.metadata.name.clone();
    if let Ok(config_json) = serde_json::to_value(&request.config) {
        if let Err(e) = state.app_state.checkpoint_db.save_config_with_id(
            &project_id,
            config_json,
            &config_name,
            "web",
            None,
        ) {
            warn!(
                "MCP API: Failed to auto-store config in ConfigStorage: {}",
                e
            );
        } else {
            info!(
                "MCP API: Auto-stored config '{}' with id '{}' in ConfigStorage",
                config_name, project_id
            );
            // Store current config ID
            if let Ok(mut current_id) = state.current_config_id.lock() {
                *current_id = Some(project_id.clone());
            }
        }
    }

    let result = ImportResult {
        success: true,
        project_id: project_id.clone(),
        message: format!(
            "Successfully imported QontinuiConfig '{}' with {} images ({} saved for RAG) and {} patterns. Embedding generation started.",
            request.config.metadata.name, image_count, saved_image_count, pattern_count
        ),
        screenshot_count: saved_image_count,
        element_count: pattern_count,
        storage_path: storage_path_str,
    };

    Ok(Json(ApiResponse::success(result)))
}

/// List all RAG configurations
pub async fn list_rag_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<RAGConfigSummary>>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Listing RAG configurations");

    let storage = state.rag_state.storage.lock().await;
    let summaries = storage.list_configs().map_err(|e| {
        error!("Failed to list RAG configs: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list configs: {}", e))),
        )
    })?;

    info!("MCP API: Found {} RAG configurations", summaries.len());

    Ok(Json(ApiResponse::success(summaries)))
}

/// Get RAG embedding status for a project
pub async fn get_rag_status(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting RAG status for project_id={}", project_id);

    let embedding_generator = state.rag_state.embedding_generator.lock().await;

    // Get progress from state if available (includes in-progress tracking)
    if let Some(progress) = embedding_generator.get_progress(&project_id) {
        let status_str = match &progress.status {
            crate::rag::EmbeddingStatus::NotStarted => "not_started",
            crate::rag::EmbeddingStatus::InProgress(_) => "in_progress",
            crate::rag::EmbeddingStatus::Completed => "completed",
            crate::rag::EmbeddingStatus::Failed(_) => "failed",
        };

        let mut data = serde_json::json!({
            "status": status_str,
            "message": progress.message,
        });

        // Add optional fields if present
        if let Some(percent) = progress.percent {
            data["percent"] = serde_json::json!(percent);
        }
        if let Some(elements_processed) = progress.elements_processed {
            data["elements_processed"] = serde_json::json!(elements_processed);
        }
        if let Some(total_elements) = progress.total_elements {
            data["total_elements"] = serde_json::json!(total_elements);
        }

        return Ok(Json(ApiResponse::success(data)));
    }

    // Fallback to file-based check (for completed/not started)
    let status = embedding_generator.check_status(&project_id);

    let status_str = match &status {
        crate::rag::EmbeddingStatus::NotStarted => "not_started",
        crate::rag::EmbeddingStatus::InProgress(pct) => {
            return Ok(Json(ApiResponse::success(serde_json::json!({
                "status": "in_progress",
                "percent": pct
            }))));
        }
        crate::rag::EmbeddingStatus::Completed => "completed",
        crate::rag::EmbeddingStatus::Failed(_) => "failed",
    };

    let message = match &status {
        crate::rag::EmbeddingStatus::Failed(err) => {
            Some(format!("Embedding generation failed: {}", err))
        }
        crate::rag::EmbeddingStatus::Completed => {
            Some("Embeddings generated successfully".to_string())
        }
        _ => None,
    };

    let mut data = serde_json::json!({
        "status": status_str
    });
    if let Some(msg) = message {
        data["message"] = serde_json::json!(msg);
    }

    Ok(Json(ApiResponse::success(data)))
}

/// Delete a RAG configuration
pub async fn delete_rag_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Deleting RAG config for project_id={}", project_id);

    let storage = state.rag_state.storage.lock().await;
    storage.delete_config(&project_id).map_err(|e| {
        error!("Failed to delete RAG config: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete config: {}", e))),
        )
    })?;

    info!(
        "MCP API: Successfully deleted RAG config for project_id={}",
        project_id
    );

    Ok(Json(ApiResponse::success(format!(
        "Successfully deleted RAG config: {}",
        project_id
    ))))
}

/// Load a RAG project into the executor
pub async fn load_rag_project(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading RAG project for project_id={}", project_id);

    // Load QontinuiConfig
    let storage = state.rag_state.storage.lock().await;

    match storage.load_qontinui_config(&project_id) {
        Ok(config) => {
            drop(storage);

            // TODO: Load into executor if needed
            Ok(Json(ApiResponse::success(serde_json::json!({
                "project_id": project_id,
                "name": config.metadata.name,
                "states": config.states.len(),
                "patterns": config.pattern_count(),
                "loaded": true
            }))))
        }
        Err(_) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Project not found: {}", project_id))),
        )),
    }
}

/// Get RAG availability (ML models status)
pub async fn get_rag_availability(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Checking RAG availability");

    // Check CLIP availability via embedding generator
    // The embedding generator uses CLIP for image embeddings
    let embedding_generator = state.rag_state.embedding_generator.lock().await;
    let clip_available = !embedding_generator.is_degraded();
    drop(embedding_generator);

    // Check text/OCR availability via semantic search
    // The semantic search uses text embeddings for search
    let semantic_search = state.rag_state.semantic_search.lock().await;
    let text_available = !semantic_search.is_degraded();
    let ocr_available = text_available; // OCR is part of the same text pipeline
    drop(semantic_search);

    // Check SAM availability via Python bridge
    // SAM3 runs via the Python executor's segment_screenshot command
    let sam_available = {
        let manager_guard = state.app_state.bridge_manager.lock().await;
        if let Some(ref manager) = *manager_guard {
            manager.is_default_bridge_running()
        } else {
            false
        }
    };

    // Overall availability: at least one model must be available
    let available = clip_available || text_available || sam_available;

    info!(
        "MCP API: RAG availability - clip={}, text={}, ocr={}, sam={}, overall={}",
        clip_available, text_available, ocr_available, sam_available, available
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "available": available,
        "models": {
            "clip": clip_available,
            "text": text_available,
            "ocr": ocr_available,
            "sam": sam_available
        }
    }))))
}

/// Segment a screenshot using SAM3 (Segment Anything Model 3)
///
/// This endpoint receives a base64-encoded screenshot and returns
/// the detected segments with their bounding boxes and cropped images.
/// SAM3 runs locally on the user's machine via the Python executor.
#[tracing::instrument(
    name = "api.request.segment_screenshot",
    skip(state, request),
    fields(
        endpoint = "/segment-screenshot",
        method = "POST",
        screenshot_size = %request.screenshot_base64.len(),
        min_area = ?request.min_area,
        model = ?request.model
    )
)]
pub async fn segment_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SegmentScreenshotRequest>,
) -> Result<Json<ApiResponse<SegmentScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Segmenting screenshot ({} bytes base64)",
        request.screenshot_base64.len()
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot_base64": request.screenshot_base64,
        "min_area": request.min_area,
        "model": request.model,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use configurable timeout for vision analysis (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::vision_analysis().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait("segment_screenshot", Some(params), timeout_duration)
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
                // Parse segments from response data
                let segments: Vec<SegmentInfo> = if let Some(data) = response.data {
                    if let Some(segments_arr) = data.get("segments").and_then(|s| s.as_array()) {
                        segments_arr
                            .iter()
                            .filter_map(|seg| {
                                let id = seg.get("id")?.as_str()?.to_string();
                                let bbox = seg.get("bbox")?.as_array()?;
                                let bbox_vec: Vec<i32> = bbox
                                    .iter()
                                    .filter_map(|v| v.as_i64().map(|n| n as i32))
                                    .collect();
                                if bbox_vec.len() != 4 {
                                    return None;
                                }
                                let area =
                                    seg.get("area").and_then(|a| a.as_i64()).unwrap_or(0) as i32;
                                let image_base64 = seg
                                    .get("image_base64")
                                    .and_then(|i| i.as_str())
                                    .map(|s| s.to_string());

                                Some(SegmentInfo {
                                    id,
                                    bbox: bbox_vec,
                                    area,
                                    image_base64,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                info!(
                    "MCP API: Segmentation completed with {} segments in {}ms",
                    segments.len(),
                    elapsed.as_millis()
                );

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: true,
                    segments,
                    error: None,
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Segmentation failed".to_string());
                error!("MCP API: Segmentation failed: {}", error_msg);

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: false,
                    segments: Vec::new(),
                    error: Some(error_msg),
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to segment screenshot: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/rag/import", post(import_rag))
        .route("/rag/list", get(list_rag_configs))
        .route("/rag/availability", get(get_rag_availability))
        .route("/rag/segment", post(segment_screenshot))
        .route("/rag/:project_id/status", get(get_rag_status))
        .route("/rag/:project_id/load", post(load_rag_project))
        .route("/rag/:project_id", delete(delete_rag_config))
}
