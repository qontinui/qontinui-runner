//! Vision-pipeline HTTP handlers under `/ui-bridge/vision/*`.
//!
//! Phase 2+3 of the UI Bridge Vision Pipeline plan
//! (`plans/2026-05-13-ui-bridge-vision-pipeline-plan.md`).
//!
//! Phase 2 replaced the deleted runner-direct screenshot/annotated-screenshot/
//! element-screenshot family with a single contract-aware capture surface
//! backed by [`qontinui_vision_core`].
//!
//! Phase 3 added the read-side cache layer, bounded-concurrency permits, and
//! mutation-keyed invalidation. The flow is now: compose cache key from
//! `(mutation_id, request shape)`; on hit, return cached bytes; on miss,
//! acquire a permit from `state.vision_capture_semaphore` (size 2) around
//! the xcap `spawn_blocking`, run the pipeline, then `vision_cache.put()`.
//! `force=true` bypasses the read-side; control handlers (click, type)
//! bump `vision_mutation_id` so subsequent cache lookups produce a fresh
//! key and re-render.

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use image::RgbaImage;
use qontinui_vision_core::{
    contract::EncodedFormat, AlphaPolicy, Annotation, AnnotationStyle, Frame, FrameSource,
    OutputContract, Pipeline, RedactKind, RedactRegion, Region, ResizeStrategy, Stage,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tracing::{debug, info, warn};

use super::screenshots::lookup_element_normalized_rect;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Request / response shapes (mirrors plan §3.2)
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionRequest {
    pub x: u32,
    pub y: u32,
    #[serde(alias = "width")]
    pub w: u32,
    #[serde(alias = "height")]
    pub h: u32,
}

impl From<RegionRequest> for Region {
    fn from(r: RegionRequest) -> Self {
        Region {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationRequest {
    pub region: RegionRequest,
    #[serde(default)]
    pub label: Option<String>,
    /// RGBA border color, e.g. `[255, 51, 51, 255]`. Defaults to `DEFAULT_RED`.
    #[serde(default)]
    pub border_color: Option<[u8; 4]>,
    /// Border thickness in pixels. Defaults to 2.
    #[serde(default)]
    pub border_thickness: Option<u32>,
    /// Optional fill color (RGBA).
    #[serde(default)]
    pub fill_color: Option<[u8; 4]>,
}

impl From<AnnotationRequest> for Annotation {
    fn from(req: AnnotationRequest) -> Self {
        let style = AnnotationStyle {
            border_color: req
                .border_color
                .unwrap_or(AnnotationStyle::DEFAULT_RED.border_color),
            border_thickness: req
                .border_thickness
                .unwrap_or(AnnotationStyle::DEFAULT_RED.border_thickness),
            fill_color: req.fill_color,
            label_color: AnnotationStyle::DEFAULT_RED.label_color,
        };
        Annotation {
            region: req.region.into(),
            label: req.label,
            style,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RedactSpecRequest {
    #[serde(rename = "blur")]
    Blur { region: RegionRequest, sigma: f32 },
    #[serde(rename = "pixelate")]
    Pixelate {
        region: RegionRequest,
        block_size: u32,
    },
    #[serde(rename = "fill")]
    Fill {
        region: RegionRequest,
        color: [u8; 4],
    },
}

impl From<RedactSpecRequest> for RedactRegion {
    fn from(req: RedactSpecRequest) -> Self {
        match req {
            RedactSpecRequest::Blur { region, sigma } => RedactRegion {
                region: region.into(),
                kind: RedactKind::Blur { sigma },
            },
            RedactSpecRequest::Pixelate { region, block_size } => RedactRegion {
                region: region.into(),
                kind: RedactKind::Pixelate { block_size },
            },
            RedactSpecRequest::Fill { region, color } => RedactRegion {
                region: region.into(),
                kind: RedactKind::Fill(color),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    /// Pixel-space rect to crop after capture.
    #[serde(default)]
    pub region: Option<RegionRequest>,
    /// Element id; resolves to a normalized rect via the runner's existing
    /// `discover` snapshot, then converted to pixel-space.
    #[serde(default)]
    pub element: Option<String>,
    /// Contract name. `"claude"` (default), `"webp"`, or `"png_strict"`.
    #[serde(default)]
    pub contract: Option<String>,
    /// Overlay rectangles + labels.
    #[serde(default)]
    pub annotations: Option<Vec<AnnotationRequest>>,
    /// Phase 3+ selector for auto-deriving annotations from elements. Phase 2
    /// returns 400 when present (see `vision/annotate`).
    #[serde(default)]
    pub annotate_elements: Option<serde_json::Value>,
    /// Per-region pixel obfuscation.
    #[serde(default)]
    pub redact: Option<Vec<RedactSpecRequest>>,
    /// Bypass cache. Phase 2 has no cache; flag is accepted for forward-compat.
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResponse {
    /// `tmp_vision_cache/<sha256>.<ext>` (relative to the runner CWD).
    pub path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    /// Lower-case extension: `"jpeg"` | `"webp"` | `"png"`.
    pub format: String,
    /// Name of the [`OutputContract`] used (`claude_vision_v1`, etc).
    pub contract: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSpec {
    #[serde(default)]
    pub region: Option<RegionRequest>,
    #[serde(default)]
    pub element: Option<String>,
    #[serde(default)]
    pub contract: Option<String>,
    #[serde(default)]
    pub annotations: Option<Vec<AnnotationRequest>>,
    #[serde(default)]
    pub redact: Option<Vec<RedactSpecRequest>>,
}

impl From<CaptureSpec> for CaptureRequest {
    fn from(s: CaptureSpec) -> Self {
        CaptureRequest {
            region: s.region,
            element: s.element,
            contract: s.contract,
            annotations: s.annotations,
            annotate_elements: None,
            redact: s.redact,
            force: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRequest {
    pub baseline: CaptureSpec,
    pub comparison: CaptureSpec,
    /// `"side_by_side"` | `"overlay"` | `"delta"`. Phase 2 honors `delta`
    /// (pixel-by-pixel diff returned as a PNG) and falls back to `delta` for
    /// the other modes — clustering / side-by-side composition is Phase 3.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffResponse {
    #[serde(flatten)]
    pub capture: CaptureResponse,
    /// Fraction of pixels that differ above the per-channel tolerance.
    pub pixel_delta_ratio: f64,
    /// Bounding rectangle of all changed pixels. Naive single-rect for Phase 2.
    pub changed_regions: Vec<RegionRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRequest {
    #[serde(default)]
    pub region: Option<RegionRequest>,
    #[serde(default)]
    pub element: Option<String>,
    /// Audit reason — required when the gate env is on.
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub pipeline_version: &'static str,
    /// Live count from `VisionCaptureSemaphore::available_permits()` — 0 to 2.
    pub available_slots: u32,
    pub cache_size_bytes: u64,
    pub cache_entry_count: usize,
    /// Cumulative since process start.
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_evictions: u64,
    pub cache_max_bytes: u64,
    /// Current value of the monotonic mutation counter. Bumped by
    /// control/click, control/type, control/navigate.
    pub mutation_id: u64,
}

// ============================================================================
// Capture helpers — xcap → image::RgbaImage → vision_core::Frame
// ============================================================================

/// Capture the runner's own window as an RGBA buffer plus the scale factor.
///
/// Mirrors the cropping logic in
/// `screenshots::capture_runner_window` (xcap captures the monitor under the
/// window, then we crop to the runner's inner-position rect), but returns the
/// raw [`RgbaImage`] instead of going through base64 PNG. Caller wraps it in
/// a [`Frame`] via [`frame_from_rgba`].
///
/// Runs on the calling thread (synchronous xcap call). Callers should wrap in
/// `tokio::task::spawn_blocking`.
fn capture_runner_window_rgba(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
) -> Result<(RgbaImage, f64), String> {
    use crate::screen::{CapturedScreenshot, MonitorManager};

    let mgr = MonitorManager::detect()?;

    let logical_x = (phys_x as f64 / scale) as i32;
    let logical_y = (phys_y as f64 / scale) as i32;
    let logical_center_x = logical_x + (phys_w as f64 / scale / 2.0) as i32;
    let logical_center_y = logical_y + (phys_h as f64 / scale / 2.0) as i32;

    let monitor = mgr
        .at_logical_point(logical_center_x, logical_center_y)
        .ok_or_else(|| "Runner window not on any monitor".to_string())?;
    let monitor_scale = monitor.scale_factor;

    let captured = CapturedScreenshot::from_monitor(&mgr, monitor.index)?;

    let (rel_local_x, rel_local_y) = monitor.to_monitor_local(logical_x, logical_y);
    let (rel_phys_x, rel_phys_y) = monitor.logical_to_physical(rel_local_x, rel_local_y);

    let crop_x = rel_phys_x.max(0) as u32;
    let crop_y = rel_phys_y.max(0) as u32;
    let crop_w = if rel_phys_x < 0 {
        phys_w.saturating_sub((-rel_phys_x) as u32)
    } else {
        phys_w
    }
    .min(captured.physical_width.saturating_sub(crop_x));
    let crop_h = if rel_phys_y < 0 {
        phys_h.saturating_sub((-rel_phys_y) as u32)
    } else {
        phys_h
    }
    .min(captured.physical_height.saturating_sub(crop_y));

    if crop_w == 0 || crop_h == 0 {
        return Err(format!(
            "Runner window has zero visible area (crop {}x{} at ({}, {}), image {}x{})",
            crop_w, crop_h, crop_x, crop_y, captured.physical_width, captured.physical_height
        ));
    }

    let cropped = captured.image.crop_imm(crop_x, crop_y, crop_w, crop_h);
    Ok((cropped.to_rgba8(), monitor_scale))
}

/// Capture the runner window and return a vision-core [`Frame`].
///
/// Acquires a permit from `state.vision_capture_semaphore` before invoking
/// xcap. xcap is GDI-bound on Windows and exhibits the "fits-2-parallel-
/// then-thrashes" pattern documented in `proj_supervisor_build_pool.md` —
/// the bounded permit pool (size 2) prevents thrash under multi-agent load.
/// Permit is held for the entire `spawn_blocking` span and released on drop.
async fn capture_runner_window_frame(state: &Arc<ApiState>) -> Result<Frame, String> {
    use tauri::Manager;

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "Runner window not found".to_string())?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = window
        .inner_position()
        .map_err(|e| format!("inner_position failed: {}", e))?;
    let size = window
        .inner_size()
        .map_err(|e| format!("inner_size failed: {}", e))?;

    let x = pos.x;
    let y = pos.y;
    let w = size.width;
    let h = size.height;

    let _permit = state
        .vision_capture_semaphore
        .acquire()
        .await
        .map_err(|e| format!("capture semaphore acquire: {}", e))?;
    let (rgba, monitor_scale) =
        tokio::task::spawn_blocking(move || capture_runner_window_rgba(x, y, w, h, scale))
            .await
            .map_err(|e| format!("capture task join error: {}", e))??;

    Ok(Frame::from_rgba(
        rgba,
        FrameSource {
            kind: qontinui_vision_core::FrameSourceKind::Window,
            scale_factor: monitor_scale,
            captured_at: chrono::Utc::now(),
        },
    ))
}

/// Compose the cache key for a capture request. Folds in the current
/// mutation_id (bumped by control/click/type/navigate) so any state-
/// changing action transparently busts cache entries — no clock-based
/// TTL, no time-based staleness window. The pipeline-shape parameters
/// (contract, region, element, annotations, redact) all participate via
/// Debug-format hashing so any parameter change flips the key.
fn compose_capture_cache_key(state: &Arc<ApiState>, req: &CaptureRequest) -> [u8; 32] {
    let mut_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    let s = format!("v=1|mut={mut_id}|req={req:?}");
    qontinui_vision_core::sha256_of(s.as_bytes())
}

/// Bump the mutation counter — call from any handler that performs a UI
/// action that could move rendered pixels (click, type, navigate). Subsequent
/// cache lookups produce a different key, so the next `vision/capture`
/// always re-renders. Uses Relaxed ordering: monotonic counter, no
/// cross-thread happens-before constraints needed.
pub fn bump_mutation_id(state: &Arc<ApiState>) {
    state
        .vision_mutation_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

// ============================================================================
// Pipeline assembly
// ============================================================================

fn resolve_contract(name: Option<&str>) -> Result<OutputContract, String> {
    match name.unwrap_or("claude").to_lowercase().as_str() {
        "claude" | "claude_vision_v1" => Ok(OutputContract::CLAUDE_VISION_V1),
        "webp" | "webp_lossy" => Ok(OutputContract::WEBP_LOSSY),
        "png_strict" | "png" => Ok(OutputContract::PNG_STRICT),
        other => Err(format!("unknown contract '{}'", other)),
    }
}

/// Pick the first allowed format from a contract. Pipeline's contract has at
/// least one entry — we just take the head.
fn pick_format(contract: &OutputContract) -> Result<EncodedFormat, String> {
    contract
        .allowed_formats
        .first()
        .copied()
        .ok_or_else(|| format!("contract {} has no allowed_formats", contract.name))
}

fn extension_for(fmt: EncodedFormat) -> &'static str {
    match fmt {
        EncodedFormat::Jpeg { .. } => "jpeg",
        EncodedFormat::Webp { .. } => "webp",
        EncodedFormat::Png => "png",
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "jpeg" | "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

/// Convert a normalized 0-1 rect into a pixel-space [`Region`] given a frame's
/// dimensions. Clamps to the frame bounds and rounds half-pixel edges away
/// from zero. Returns `None` if the resulting region is empty.
fn normalized_to_region(
    rect: &crate::vision::types::NormalizedRect,
    width: u32,
    height: u32,
) -> Option<Region> {
    let img_w = width as f64;
    let img_h = height as f64;
    let x = (rect.x as f64 * img_w).round().max(0.0) as u32;
    let y = (rect.y as f64 * img_h).round().max(0.0) as u32;
    let w = (rect.width as f64 * img_w).round().max(0.0) as u32;
    let h = (rect.height as f64 * img_h).round().max(0.0) as u32;
    let w = w.min(width.saturating_sub(x));
    let h = h.min(height.saturating_sub(y));
    if w == 0 || h == 0 {
        return None;
    }
    Some(Region { x, y, w, h })
}

/// Resolve the optional crop region from a [`CaptureRequest`].
async fn resolve_crop_region(
    state: &Arc<ApiState>,
    region: &Option<RegionRequest>,
    element: &Option<String>,
    frame_w: u32,
    frame_h: u32,
) -> Result<Option<Region>, (StatusCode, String)> {
    if let Some(rr) = region {
        let r: Region = rr.clone().into();
        if !r.fits_in(frame_w, frame_h) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "region ({},{}) {}x{} does not fit in frame {}x{}",
                    r.x, r.y, r.w, r.h, frame_w, frame_h
                ),
            ));
        }
        return Ok(Some(r));
    }
    if let Some(id) = element {
        let rect = lookup_element_normalized_rect(state, id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("element_not_found: '{}'", id),
                )
            })?;
        let region = normalized_to_region(&rect, frame_w, frame_h).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "element '{}' resolved to zero-area rect (rect=({:.3},{:.3},{:.3},{:.3}), \
                     frame {}x{})",
                    id, rect.x, rect.y, rect.width, rect.height, frame_w, frame_h
                ),
            )
        })?;
        return Ok(Some(region));
    }
    Ok(None)
}

/// Build a pipeline honoring the requested contract.
fn build_pipeline(
    contract: OutputContract,
    crop: Option<Region>,
    annotations: Vec<Annotation>,
    redact: Vec<RedactRegion>,
) -> Pipeline {
    let mut p = Pipeline::new();
    if let Some(r) = crop {
        p = p.push(Stage::CropRegion(r));
    }
    if let AlphaPolicy::Flatten(bg) = contract.alpha_policy {
        p = p.push(Stage::FlattenAlpha(bg));
    }
    if contract.max_long_edge != u32::MAX {
        p = p.push(Stage::Resize(ResizeStrategy::LongEdge(
            contract.max_long_edge,
        )));
    }
    if !annotations.is_empty() {
        p = p.push(Stage::Annotate(annotations));
    }
    if !redact.is_empty() {
        p = p.push(Stage::Redact(redact));
    }
    p = p.push(Stage::StripMetadata);
    let format = pick_format(&contract).expect("contract has at least one format");
    p = p.push(Stage::Encode(format));
    p = p.push(Stage::Verify(contract));
    p
}

/// Build a contract-free pipeline for `vision/raw` (no Verify, raw PNG).
fn build_raw_pipeline(crop: Option<Region>) -> Pipeline {
    let mut p = Pipeline::new();
    if let Some(r) = crop {
        p = p.push(Stage::CropRegion(r));
    }
    p = p.push(Stage::Encode(EncodedFormat::Png));
    p
}

// ============================================================================
// Cache directory + atomic write
// ============================================================================

fn cache_dir() -> PathBuf {
    PathBuf::from("tmp_vision_cache")
}

fn ensure_cache_dir() -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// Atomically write `bytes` into `<dir>/<sha>.<ext>` via
/// `NamedTempFile::persist`.
fn persist_atomic(dir: &StdPath, sha: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let target = dir.join(format!("{}.{}", sha, ext));
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("tempfile create: {}", e))?;
    tmp.write_all(bytes)
        .map_err(|e| format!("tempfile write: {}", e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| format!("tempfile sync: {}", e))?;
    tmp.persist(&target)
        .map_err(|e| format!("tempfile persist: {}", e.error))?;
    Ok(target)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

// ============================================================================
// Handlers
// ============================================================================

/// `POST /ui-bridge/vision/capture` — replaces `/control/screenshot`.
async fn vision_capture_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<CaptureRequest>>,
) -> Result<Json<ApiResponse<CaptureResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    do_capture(&state, req, false).await
}

/// `POST /ui-bridge/vision/annotate` — alias for `capture` with an explicit
/// annotations array. Phase 2 rejects the `annotate_elements` selector form
/// (auto-deriving annotations from a `discover` snapshot is Phase 3).
async fn vision_annotate_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<CaptureRequest>>,
) -> Result<Json<ApiResponse<CaptureResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    if req.annotate_elements.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "annotate_elements selector is not implemented in Phase 2 — \
                 pass an explicit `annotations` array instead",
            )),
        ));
    }
    do_capture(&state, req, true).await
}

async fn do_capture(
    state: &Arc<ApiState>,
    req: CaptureRequest,
    require_annotations: bool,
) -> Result<Json<ApiResponse<CaptureResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if require_annotations
        && req
            .annotations
            .as_ref()
            .map(|a| a.is_empty())
            .unwrap_or(true)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "vision/annotate requires a non-empty annotations array",
            )),
        ));
    }

    let contract = resolve_contract(req.contract.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    let format = pick_format(&contract).expect("contract has at least one format");
    let ext = extension_for(format);

    // Compose cache key from (mutation_id, request shape). Mutation_id bumps
    // on every UI-changing action so cache entries auto-invalidate.
    let force = req.force.unwrap_or(false);
    let cache_key = compose_capture_cache_key(state, &req);

    // Try cache before capturing. Cache hit → skip xcap + pipeline entirely.
    if !force {
        if let Some(hit) = state.vision_cache.get(&cache_key) {
            let bytes = std::fs::read(&hit.path).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("read cached file: {}", e))),
                )
            })?;
            let decoded = image::load_from_memory(&bytes).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("decode cached: {}", e))),
                )
            })?;
            debug!(
                "vision/capture: cache HIT key={} bytes={}",
                &hit.sha256_hex[..12],
                bytes.len()
            );
            return Ok(Json(ApiResponse::success(CaptureResponse {
                path: hit.path.to_string_lossy().into_owned(),
                sha256: hit.sha256_hex,
                width: decoded.width(),
                height: decoded.height(),
                bytes: bytes.len(),
                format: ext.to_string(),
                contract: contract.name.to_string(),
            })));
        }
    }

    let frame = capture_runner_window_frame(state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let crop = resolve_crop_region(state, &req.region, &req.element, frame.width, frame.height)
        .await
        .map_err(|(code, msg)| (code, Json(api_error(msg))))?;

    let annotations: Vec<Annotation> = req
        .annotations
        .unwrap_or_default()
        .into_iter()
        .map(Into::into)
        .collect();
    let redact: Vec<RedactRegion> = req
        .redact
        .unwrap_or_default()
        .into_iter()
        .map(Into::into)
        .collect();

    let pipeline = build_pipeline(contract, crop, annotations, redact);

    let bytes = pipeline.run(frame).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    // Decode for final w/h. We already verified the bytes against the contract.
    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("decode: {}", e))),
        )
    })?;
    let width = decoded.width();
    let height = decoded.height();

    let hit = state
        .vision_cache
        .put(&cache_key, &bytes, ext)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("cache put: {}", e))),
            )
        })?;

    info!(
        "vision/capture: cache MISS key={} contract={} format={} bytes={} {}x{}",
        &hit.sha256_hex[..12],
        contract.name,
        ext,
        bytes.len(),
        width,
        height
    );

    Ok(Json(ApiResponse::success(CaptureResponse {
        path: hit.path.to_string_lossy().into_owned(),
        sha256: hit.sha256_hex,
        width,
        height,
        bytes: bytes.len(),
        format: ext.to_string(),
        contract: contract.name.to_string(),
    })))
}

/// `POST /ui-bridge/vision/diff` — capture twice + naive pixel diff.
async fn vision_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DiffRequest>,
) -> Result<Json<ApiResponse<DiffResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mode = req.mode.as_deref().unwrap_or("delta").to_lowercase();
    if !["delta", "overlay", "side_by_side"].contains(&mode.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("unknown diff mode '{}'", mode))),
        ));
    }

    let baseline_req: CaptureRequest = req.baseline.into();
    let comparison_req: CaptureRequest = req.comparison.into();

    // We need both raw RGBA buffers to compute a meaningful diff before encoding.
    // Run each spec's crop + alpha-flatten + resize but skip the encoder, then
    // diff in pixel space and re-encode the delta image through the comparison
    // contract.
    let baseline_frame = produce_intermediate_frame(&state, &baseline_req).await?;
    let comparison_frame = produce_intermediate_frame(&state, &comparison_req).await?;

    if baseline_frame.width != comparison_frame.width
        || baseline_frame.height != comparison_frame.height
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "baseline {}x{} differs from comparison {}x{} — diff requires equal dimensions",
                baseline_frame.width,
                baseline_frame.height,
                comparison_frame.width,
                comparison_frame.height
            ))),
        ));
    }

    let (delta_image, ratio, bbox) = compute_pixel_delta(&baseline_frame, &comparison_frame);

    // Persist the delta PNG through a fresh pipeline so we get the standard
    // capture envelope back.
    let contract = resolve_contract(comparison_req.contract.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    let format = pick_format(&contract).expect("contract has at least one format");
    let ext = extension_for(format);

    let delta_frame = Frame::from_rgba(
        delta_image,
        FrameSource {
            kind: qontinui_vision_core::FrameSourceKind::Synthetic,
            scale_factor: comparison_frame.source.scale_factor,
            captured_at: chrono::Utc::now(),
        },
    );

    let pipeline = build_pipeline(contract, None, Vec::new(), Vec::new());
    let bytes = pipeline.run(delta_frame).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("decode: {}", e))),
        )
    })?;

    let sha = sha256_hex(&bytes);
    let dir =
        ensure_cache_dir().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let path = persist_atomic(&dir, &sha, ext, &bytes)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let changed_regions = bbox
        .map(|r| {
            vec![RegionRequest {
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
            }]
        })
        .unwrap_or_default();

    info!(
        "vision/diff: mode={} ratio={:.4} sha={} bytes={}",
        mode,
        ratio,
        &sha[..12],
        bytes.len()
    );

    Ok(Json(ApiResponse::success(DiffResponse {
        capture: CaptureResponse {
            path: path.to_string_lossy().into_owned(),
            sha256: sha,
            width: decoded.width(),
            height: decoded.height(),
            bytes: bytes.len(),
            format: ext.to_string(),
            contract: contract.name.to_string(),
        },
        pixel_delta_ratio: ratio,
        changed_regions,
    })))
}

/// Produce an intermediate [`Frame`] reflecting a `CaptureSpec`'s crop + alpha
/// flattening + resize, without encoding. Used by `vision/diff` so we can
/// compare in pixel space.
async fn produce_intermediate_frame(
    state: &Arc<ApiState>,
    req: &CaptureRequest,
) -> Result<Frame, (StatusCode, Json<ApiResponse<()>>)> {
    let contract = resolve_contract(req.contract.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;

    let frame = capture_runner_window_frame(state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let crop = resolve_crop_region(state, &req.region, &req.element, frame.width, frame.height)
        .await
        .map_err(|(code, msg)| (code, Json(api_error(msg))))?;

    // Build a *frame-only* pipeline: stages stop short of encode.
    // We can't actually call Pipeline::run because it must terminate in an
    // Encode. Inline the same steps manually.
    let working = if let Some(r) = crop {
        crop_in_place(frame, r)?
    } else {
        frame
    };
    let working = match contract.alpha_policy {
        AlphaPolicy::Flatten(bg) => flatten_in_place(working, bg),
        AlphaPolicy::Preserve => working,
    };
    let working = if contract.max_long_edge != u32::MAX {
        resize_long_edge(working, contract.max_long_edge)
    } else {
        working
    };
    Ok(working)
}

fn crop_in_place(
    frame: Frame,
    region: Region,
) -> Result<Frame, (StatusCode, Json<ApiResponse<()>>)> {
    if !region.fits_in(frame.width, frame.height) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "crop region {:?} does not fit in frame {}x{}",
                region, frame.width, frame.height
            ))),
        ));
    }
    let dyn_img = image::DynamicImage::ImageRgba8(frame.buffer);
    let cropped = dyn_img.crop_imm(region.x, region.y, region.w, region.h);
    Ok(Frame::from_rgba(cropped.to_rgba8(), frame.source))
}

fn flatten_in_place(frame: Frame, bg: [u8; 3]) -> Frame {
    let mut out = RgbaImage::new(frame.width, frame.height);
    let br = bg[0] as u16;
    let bgc = bg[1] as u16;
    let bb = bg[2] as u16;
    for (x, y, px) in frame.buffer.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let af = a as u16;
        let inv = 255u16 - af;
        let nr = ((r as u16 * af + br * inv) / 255) as u8;
        let ng = ((g as u16 * af + bgc * inv) / 255) as u8;
        let nb = ((b as u16 * af + bb * inv) / 255) as u8;
        out.put_pixel(x, y, image::Rgba([nr, ng, nb, 0xFF]));
    }
    Frame::from_rgba(out, frame.source)
}

fn resize_long_edge(frame: Frame, max: u32) -> Frame {
    let long = frame.width.max(frame.height);
    if long <= max || long == 0 {
        return frame;
    }
    let ratio = max as f64 / long as f64;
    let w = ((frame.width as f64 * ratio).round() as u32).max(1);
    let h = ((frame.height as f64 * ratio).round() as u32).max(1);
    let dyn_img = image::DynamicImage::ImageRgba8(frame.buffer);
    let resized = dyn_img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
    Frame::from_rgba(resized.to_rgba8(), frame.source)
}

/// Naive single-pass pixel diff. Produces a "delta" RGBA buffer where
/// changed pixels are highlighted red and unchanged pixels copy the
/// comparison frame at 50% brightness. Returns the delta image, the
/// changed-pixel ratio, and a single naive bounding rect of all changed
/// pixels (`None` if nothing changed). Phase 3 will add clustering and the
/// side_by_side / overlay layouts.
fn compute_pixel_delta(a: &Frame, b: &Frame) -> (RgbaImage, f64, Option<Region>) {
    let w = a.width;
    let h = a.height;
    let mut out = RgbaImage::new(w, h);

    let mut changed: u64 = 0;
    let mut total: u64 = 0;
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;

    for y in 0..h {
        for x in 0..w {
            total += 1;
            let pa = a.buffer.get_pixel(x, y).0;
            let pb = b.buffer.get_pixel(x, y).0;
            let diff: u32 = (0..3)
                .map(|c| (pa[c] as i32 - pb[c] as i32).unsigned_abs())
                .sum();
            if diff > 24 {
                changed += 1;
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
                out.put_pixel(x, y, image::Rgba([0xFF, 0x33, 0x33, 0xFF]));
            } else {
                let half = [pb[0] / 2, pb[1] / 2, pb[2] / 2, 0xFF];
                out.put_pixel(x, y, image::Rgba(half));
            }
        }
    }

    let ratio = if total == 0 {
        0.0
    } else {
        changed as f64 / total as f64
    };

    let bbox = if changed == 0 {
        None
    } else {
        Some(Region {
            x: min_x,
            y: min_y,
            w: max_x.saturating_sub(min_x).saturating_add(1),
            h: max_y.saturating_sub(min_y).saturating_add(1),
        })
    };

    (out, ratio, bbox)
}

/// `POST /ui-bridge/vision/raw` — unsanitized capture. Gated on
/// `QONTINUI_VISION_RAW=1`. When the env is not `"1"`, returns `404` with
/// no body (invisible, not 403, per plan §3.2).
async fn vision_raw_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<RawRequest>>,
) -> Result<Json<ApiResponse<CaptureResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let gate = std::env::var("QONTINUI_VISION_RAW")
        .ok()
        .filter(|v| v == "1");
    if gate.is_none() {
        // Plan: "invisible, not 403". 404 with no body — caller can't tell
        // whether the route exists.
        return Err((StatusCode::NOT_FOUND, Json(api_error(""))));
    }

    let req = body.map(|b| b.0).unwrap_or(RawRequest {
        region: None,
        element: None,
        reason: None,
    });
    let reason = req.reason.unwrap_or_default();
    if !matches!(reason.as_str(), "vga_grounding" | "regression_baseline") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "vision/raw requires `reason` field: \"vga_grounding\" | \"regression_baseline\"",
            )),
        ));
    }

    info!(
        target: "vision_raw_audit",
        reason = %reason,
        element = req.element.as_deref().unwrap_or(""),
        "vision/raw invoked (Phase 2: tracing audit; PG audit table is Phase 3)"
    );

    let frame = capture_runner_window_frame(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let crop = resolve_crop_region(&state, &req.region, &req.element, frame.width, frame.height)
        .await
        .map_err(|(code, msg)| (code, Json(api_error(msg))))?;

    let pipeline = build_raw_pipeline(crop);
    let bytes = pipeline.run(frame).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("decode: {}", e))),
        )
    })?;

    let sha = sha256_hex(&bytes);
    let dir =
        ensure_cache_dir().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let path = persist_atomic(&dir, &sha, "png", &bytes)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(CaptureResponse {
        path: path.to_string_lossy().into_owned(),
        sha256: sha,
        width: decoded.width(),
        height: decoded.height(),
        bytes: bytes.len(),
        format: "png".to_string(),
        contract: "raw".to_string(),
    })))
}

/// `GET /ui-bridge/vision/cache/{sha256}` — stream a cached image.
async fn vision_cache_get_handler(
    Path(sha): Path<String>,
) -> Result<Response, (StatusCode, Json<ApiResponse<()>>)> {
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) || sha.len() != 64 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("invalid sha256 (must be 64 hex chars)")),
        ));
    }
    let dir = cache_dir();
    if !dir.exists() {
        return Err((StatusCode::NOT_FOUND, Json(api_error("cache empty"))));
    }
    let entries = std::fs::read_dir(&dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("readdir: {}", e))),
        )
    })?;
    let prefix = format!("{}.", sha);
    let mut hit: Option<(PathBuf, String)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(ext) = name.strip_prefix(&prefix) {
            hit = Some((entry.path(), ext.to_string()));
            break;
        }
    }
    let (path, ext) = hit.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("no cache entry for {}", sha))),
        )
    })?;
    let bytes = std::fs::read(&path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("read: {}", e))),
        )
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(mime_for(&ext)),
    );

    debug!(
        "vision/cache GET sha={} ext={} bytes={}",
        sha,
        ext,
        bytes.len()
    );

    Ok((StatusCode::OK, headers, Body::from(bytes)).into_response())
}

/// `GET /ui-bridge/vision/health` — pipeline + cache health.
async fn vision_health_handler(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<HealthResponse>> {
    let stats = state.vision_cache.stats();
    let permits = state.vision_capture_semaphore.available_permits() as u32;
    let mutation_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);

    Json(ApiResponse::success(HealthResponse {
        pipeline_version: "0.1.1",
        available_slots: permits,
        cache_size_bytes: stats.total_bytes,
        cache_entry_count: stats.entries,
        cache_hits: stats.hits,
        cache_misses: stats.misses,
        cache_evictions: stats.evictions,
        cache_max_bytes: stats.max_bytes,
        mutation_id,
    }))
}

fn compute_cache_stats(dir: &StdPath) -> (u64, usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };
    let mut total: u64 = 0;
    let mut count = 0usize;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
                count += 1;
            }
        }
    }
    if count == 0 {
        warn!("vision_cache dir {} present but empty", dir.display());
    }
    (total, count)
}

// ============================================================================
// Router + manifest
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/vision/capture", post(vision_capture_handler))
        .route("/ui-bridge/vision/annotate", post(vision_annotate_handler))
        .route("/ui-bridge/vision/diff", post(vision_diff_handler))
        .route("/ui-bridge/vision/raw", post(vision_raw_handler))
        .route(
            "/ui-bridge/vision/cache/{sha256}",
            get(vision_cache_get_handler),
        )
        .route("/ui-bridge/vision/health", get(vision_health_handler))
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/vision/capture"),
        ("POST", "/ui-bridge/vision/annotate"),
        ("POST", "/ui-bridge/vision/diff"),
        ("POST", "/ui-bridge/vision/raw"),
        ("GET", "/ui-bridge/vision/cache/{sha256}"),
        ("GET", "/ui-bridge/vision/health"),
    ]
}
