//! Image Quality Tests HTTP API handlers
//!
//! Dev-mode endpoints for managing image quality test images and manifests.
//! Used by the Image Quality Tests page in the runner frontend.
//!
//! Endpoints:
//! - GET  /image-quality-tests/manifest           — returns the manifest JSON
//! - GET  /image-quality-tests/image/:path         — returns image as base64 JSON
//! - POST /image-quality-tests/upload              — upload a new test image
//! - DELETE /image-quality-tests/image/:path       — delete a test image

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SourceDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TestImageEntry {
    pub file: String,
    pub expected_label: String,
    #[serde(default)]
    pub expected_text: Option<String>,
    pub issue: String,
    pub description: String,
    #[serde(default)]
    pub element_type: Option<String>,
    #[serde(default)]
    pub source_dimensions: Option<SourceDimensions>,
    #[serde(default)]
    pub expected_score_range: Option<[f64; 2]>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub images: Vec<TestImageEntry>,
}

#[derive(Debug, Deserialize)]
pub struct UploadRequest {
    pub category: String,
    pub filename: String,
    pub base64_png: String,
    pub expected_label: String,
    pub description: String,
    pub issue: String,
    #[serde(default)]
    pub element_type: Option<String>,
    #[serde(default)]
    pub expected_text: Option<String>,
}

// ============================================================================
// Helpers
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct UpdateImageRequest {
    pub expected_label: Option<String>,
    pub expected_text: Option<String>,
    pub issue: Option<String>,
    pub description: Option<String>,
    pub element_type: Option<String>,
    pub expected_score_range: Option<[f64; 2]>,
    /// Move the image to a different category (moves the file on disk)
    pub category: Option<String>,
}

/// Valid image categories
const VALID_CATEGORIES: &[&str] = &[
    "good",
    "clipped",
    "invisible-text",
    "wrong-content",
    "blank",
    "shifted",
];

/// Resolve the base path for image-quality-tests directory.
/// Walks up from the runner directory to find the parent workspace,
/// then looks for image-quality-tests.
fn get_base_path() -> Result<std::path::PathBuf, String> {
    let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root)
        .map_err(|e| format!("Failed to get workspace root: {}", e))?;

    let base = workspace_root.join("image-quality-tests");
    if base.exists() {
        Ok(base)
    } else {
        Err(format!(
            "image-quality-tests directory not found at: {}",
            base.display()
        ))
    }
}

fn read_manifest(base_path: &std::path::Path) -> Result<Manifest, String> {
    let manifest_path = base_path.join("manifests").join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Manifest {
            version: "1.0".to_string(),
            description: Some("Test images for UI Bridge image quality scoring".to_string()),
            images: vec![],
        });
    }
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse manifest: {}", e))
}

fn write_manifest(base_path: &std::path::Path, manifest: &Manifest) -> Result<(), String> {
    let manifest_dir = base_path.join("manifests");
    std::fs::create_dir_all(&manifest_dir)
        .map_err(|e| format!("Failed to create manifests directory: {}", e))?;
    let manifest_path = manifest_dir.join("manifest.json");
    let content = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    std::fs::write(&manifest_path, content).map_err(|e| format!("Failed to write manifest: {}", e))
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /image-quality-tests/manifest
/// Returns the manifest JSON with all test image metadata.
async fn get_manifest(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Manifest>>, (StatusCode, Json<ApiResponse<()>>)> {
    let base_path = get_base_path().map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::NOT_FOUND, Json(api_error(e)))
    })?;

    let manifest = read_manifest(&base_path).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    Ok(Json(ApiResponse::success(manifest)))
}

/// GET /image-quality-tests/image/:category/:filename
/// Returns the image as base64-encoded JSON.
async fn get_image(
    State(_state): State<Arc<ApiState>>,
    Path((category, filename)): Path<(String, String)>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let image_path = format!("{}/{}", category, filename);
    let base_path = get_base_path().map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::NOT_FOUND, Json(api_error(e)))
    })?;

    let full_path = base_path.join("test-images").join(&image_path);

    // Security: ensure the resolved path is within our base
    let canonical = full_path.canonicalize().map_err(|e| {
        warn!(
            "Image quality tests: Failed to canonicalize path {}: {}",
            full_path.display(),
            e
        );
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Image not found: {}", image_path))),
        )
    })?;

    let canonical_base = base_path.join("test-images").canonicalize().map_err(|e| {
        error!("Image quality tests: Failed to canonicalize base: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Internal error".to_string())),
        )
    })?;

    if !canonical.starts_with(&canonical_base) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(api_error("Path traversal not allowed".to_string())),
        ));
    }

    let bytes = std::fs::read(&canonical).map_err(|e| {
        warn!(
            "Image quality tests: Failed to read image {}: {}",
            image_path, e
        );
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Image not found: {}", image_path))),
        )
    })?;

    let base64_data = BASE64.encode(&bytes);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "path": image_path,
        "base64": base64_data,
        "size_bytes": bytes.len(),
    }))))
}

/// POST /image-quality-tests/upload
/// Upload a new test image and add it to the manifest.
async fn upload_image(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<UploadRequest>,
) -> Result<Json<ApiResponse<TestImageEntry>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate category
    if !VALID_CATEGORIES.contains(&req.category.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Invalid category '{}'. Valid categories: {:?}",
                req.category, VALID_CATEGORIES
            ))),
        ));
    }

    // Validate filename (no path traversal)
    if req.filename.contains('/') || req.filename.contains('\\') || req.filename.contains("..") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Filename must not contain path separators or '..'".to_string(),
            )),
        ));
    }

    let base_path = get_base_path().map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::NOT_FOUND, Json(api_error(e)))
    })?;

    // Decode base64
    let image_bytes = BASE64.decode(&req.base64_png).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid base64 data: {}", e))),
        )
    })?;

    // Create category directory if needed
    let category_dir = base_path.join("test-images").join(&req.category);
    std::fs::create_dir_all(&category_dir).map_err(|e| {
        error!("Image quality tests: Failed to create directory: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create directory: {}", e))),
        )
    })?;

    // Write image file
    let image_path = category_dir.join(&req.filename);
    std::fs::write(&image_path, &image_bytes).map_err(|e| {
        error!("Image quality tests: Failed to write image: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write image: {}", e))),
        )
    })?;

    // Create manifest entry
    let relative_path = format!("{}/{}", req.category, req.filename);
    let entry = TestImageEntry {
        file: relative_path,
        expected_label: req.expected_label,
        expected_text: req.expected_text,
        issue: req.issue,
        description: req.description,
        element_type: req.element_type,
        source_dimensions: None,
        expected_score_range: None,
    };

    // Update manifest
    let mut manifest = read_manifest(&base_path).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    // Remove existing entry with same file path if present
    manifest.images.retain(|img| img.file != entry.file);
    manifest.images.push(entry.clone());

    write_manifest(&base_path, &manifest).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    info!(
        "Image quality tests: Uploaded image {} ({} bytes)",
        entry.file,
        image_bytes.len()
    );

    Ok(Json(ApiResponse::success(entry)))
}

/// PUT /image-quality-tests/image/:category/:filename
/// Update a test image's manifest entry metadata (label, issue, description, etc.).
/// Does NOT change the image file itself — only the manifest entry.
async fn update_image(
    State(_state): State<Arc<ApiState>>,
    Path((category, filename)): Path<(String, String)>,
    Json(req): Json<UpdateImageRequest>,
) -> Result<Json<ApiResponse<TestImageEntry>>, (StatusCode, Json<ApiResponse<()>>)> {
    let image_path = format!("{}/{}", category, filename);
    let base_path = get_base_path().map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::NOT_FOUND, Json(api_error(e)))
    })?;

    let mut manifest = read_manifest(&base_path).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    // Find the entry to update
    let entry = manifest
        .images
        .iter_mut()
        .find(|img| img.file == image_path)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!(
                    "Image not found in manifest: {}",
                    image_path
                ))),
            )
        })?;

    // Apply updates — only update fields that are provided (Some)
    if let Some(label) = req.expected_label {
        entry.expected_label = label;
    }
    if let Some(text) = req.expected_text {
        entry.expected_text = Some(text);
    }
    if let Some(issue) = req.issue {
        entry.issue = issue;
    }
    if let Some(description) = req.description {
        entry.description = description;
    }
    if let Some(element_type) = req.element_type {
        entry.element_type = Some(element_type);
    }
    if let Some(score_range) = req.expected_score_range {
        entry.expected_score_range = Some(score_range);
    }

    // Handle category change (moves the file on disk)
    if let Some(ref new_category) = req.category {
        if !VALID_CATEGORIES.contains(&new_category.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Invalid category: {}", new_category))),
            ));
        }
        if new_category != &category {
            let old_path = base_path.join("test-images").join(&image_path);
            let new_dir = base_path.join("test-images").join(new_category);
            std::fs::create_dir_all(&new_dir).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to create category dir: {}", e))),
                )
            })?;
            let new_path = new_dir.join(&filename);
            std::fs::rename(&old_path, &new_path).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to move image: {}", e))),
                )
            })?;
            entry.file = format!("{}/{}", new_category, filename);
            info!(
                "Image quality tests: Moved {} -> {}",
                image_path, entry.file
            );
        }
    }

    let updated = entry.clone();

    write_manifest(&base_path, &manifest).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    info!("Image quality tests: Updated metadata for {}", image_path);

    Ok(Json(ApiResponse::success(updated)))
}

/// DELETE /image-quality-tests/image/:category/:filename
/// Delete a test image and remove it from the manifest.
async fn delete_image(
    State(_state): State<Arc<ApiState>>,
    Path((category, filename)): Path<(String, String)>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let image_path = format!("{}/{}", category, filename);
    let base_path = get_base_path().map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::NOT_FOUND, Json(api_error(e)))
    })?;

    let full_path = base_path.join("test-images").join(&image_path);

    // Security check
    if let Ok(canonical) = full_path.canonicalize() {
        if let Ok(canonical_base) = base_path.join("test-images").canonicalize() {
            if !canonical.starts_with(&canonical_base) {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(api_error("Path traversal not allowed".to_string())),
                ));
            }
        }
    }

    // Delete the file
    if full_path.exists() {
        std::fs::remove_file(&full_path).map_err(|e| {
            error!("Image quality tests: Failed to delete image: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to delete image: {}", e))),
            )
        })?;
    }

    // Update manifest
    let mut manifest = read_manifest(&base_path).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    manifest.images.retain(|img| img.file != image_path);
    write_manifest(&base_path, &manifest).map_err(|e| {
        error!("Image quality tests: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;

    info!("Image quality tests: Deleted image {}", image_path);

    Ok(Json(ApiResponse::success(format!(
        "Deleted: {}",
        image_path
    ))))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/image-quality-tests/manifest", get(get_manifest))
        .route(
            "/image-quality-tests/image/{category}/{filename}",
            get(get_image).put(update_image).delete(delete_image),
        )
        .route("/image-quality-tests/upload", post(upload_image))
}
