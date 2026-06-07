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
use super::vision_ai::{self, OcrClient, VlmClient};
use super::vision_frame_source::resolve_frame_provider;
use crate::mcp::envelope::{RequestHints, UiBridgeJson};
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
    /// Bypass the read-side cache (still updates on write).
    #[serde(default)]
    pub force: Option<bool>,
    /// Multi-output fan-out (plan §3.4): a single xcap capture feeds N
    /// derived pipelines. When present, the top-level capture fields
    /// (region, element, etc) are ignored and the response shape becomes
    /// `{ captures: { name: CaptureResponse } }`.
    #[serde(default)]
    pub captures: Option<Vec<NamedCapture>>,
    /// Optional frame source. `None` (default) captures the runner's own
    /// desktop window (legacy behavior). A device/app id sources the frame
    /// from that target instead — see [`super::vision_frame_source`].
    /// Participates in the cache key via the request's `Debug` formatting.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedCapture {
    pub name: String,
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

impl From<&NamedCapture> for CaptureRequest {
    fn from(n: &NamedCapture) -> Self {
        CaptureRequest {
            region: n.region.clone(),
            element: n.element.clone(),
            contract: n.contract.clone(),
            annotations: n.annotations.clone(),
            annotate_elements: None,
            redact: n.redact.clone(),
            force: None,
            captures: None,
            // Parent `target` is threaded through `do_multi_capture` separately,
            // not via NamedCapture (which carries no per-capture target).
            target: None,
        }
    }
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
    /// Capture backend that produced the underlying frame, when known.
    /// `"Webview2CapturePreview"` | `"MonitorCrop"`; `None` for device /
    /// synthetic frames where no runner-window backend applies.
    #[serde(rename = "captureBackend", skip_serializing_if = "Option::is_none")]
    pub capture_backend: Option<String>,
}

/// Response envelope for `vision/capture` and `vision/annotate`. Single-output
/// requests get a flat [`CaptureResponse`]; multi-output requests (with
/// `captures: [...]` in the body) get `{ captures: { name: CaptureResponse } }`.
/// Untagged serialization → clients dispatch on presence of `captures` key.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum VisionCaptureResp {
    Single(CaptureResponse),
    Multi {
        captures: std::collections::HashMap<String, CaptureResponse>,
    },
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
            captures: None,
            // `CaptureSpec` carries no target; callers (diff, baseline) set it
            // explicitly on the produced `CaptureRequest` when needed.
            target: None,
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
    /// Optional frame source for *both* baseline and comparison captures.
    /// `None` (default) = runner desktop. See [`super::vision_frame_source`].
    #[serde(default)]
    pub target: Option<String>,
}

impl RequestHints for DiffRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required fields: `baseline` and `comparison` (CaptureSpec objects with optional \
             `region`, `element`, `contract`, `annotations`, `redact`). \
             Optional: `mode` (default \"delta\"), `target` (device/app id)."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "allowedModes": ["delta", "overlay", "side_by_side"] }))
    }
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

/// `POST /ui-bridge/vision/extract` request shape (plan §3.2). OCR
/// extraction — image goes to a model, only text + bbox come back.
/// No pixels in the response.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    #[serde(default)]
    pub region: Option<RegionRequest>,
    #[serde(default)]
    pub element: Option<String>,
    /// Language hint; passed through to the model. Empty / unset = model
    /// default (usually English-biased).
    #[serde(default)]
    pub lang: Option<String>,
    /// Drop blocks below this confidence. Default 0.5.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Bypass the read-side cache.
    #[serde(default)]
    pub force: Option<bool>,
    /// Optional frame source. `None` (default) = runner desktop. See
    /// [`super::vision_frame_source`]. Participates in the cache key.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResponse {
    pub blocks: Vec<vision_ai::OcrBlock>,
    /// Block texts joined by newline in scan order (top-to-bottom). Useful
    /// for `contains` / `regex` searches without walking the bbox list.
    pub aggregate_text: String,
    /// Model alias the request was routed to (after env-var resolution).
    pub model: String,
    /// True iff we read from cache instead of calling the model.
    pub cached: bool,
    /// Capture backend that produced the underlying frame, when known.
    /// `"Webview2CapturePreview"` | `"MonitorCrop"`; `None` for device /
    /// synthetic frames where no runner-window backend applies.
    #[serde(
        rename = "captureBackend",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub capture_backend: Option<String>,
}

/// `POST /ui-bridge/vision/describe` request shape (plan §3.2). VLM
/// caption / Q&A — same no-pixels-in-response contract as extract.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeRequest {
    #[serde(default)]
    pub region: Option<RegionRequest>,
    #[serde(default)]
    pub element: Option<String>,
    /// Optional addendum to the canonical VLM system prompt. e.g.,
    /// `"Focus on the terminal area."`. The agent's actual question
    /// can also be phrased here.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Caller-cap on caption length. Default 256.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub force: Option<bool>,
    /// Optional frame source. `None` (default) = runner desktop. See
    /// [`super::vision_frame_source`]. Participates in the cache key.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeResponse {
    /// Human-readable caption. Retained as a deliberate dual-audience
    /// feature (plan goal #3) — byte-unchanged contract vs. pre-Phase-4.
    pub description: String,
    /// Closed-schema machine twin of `description` (plan §8 Phase 4).
    /// `None` when the VLM reply was prose-only or failed strict
    /// validation; `description` is still populated in that case
    /// (graceful fallback, `UB-VLM-STRUCTURED-PARSE-FAIL` logged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<vision_ai::VlmStructuredSummary>,
    pub tokens: Option<vision_ai::VlmTokens>,
    pub model: String,
    pub cached: bool,
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
    /// Optional frame source. `None` (default) = runner desktop. See
    /// [`super::vision_frame_source`]. Participates in the cache key.
    #[serde(default)]
    pub target: Option<String>,
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
    /// Cumulative runner-window frames served by the WebView2 CapturePreview
    /// backend since process start.
    pub vision_capture_preview_count: u64,
    /// Cumulative runner-window frames served by the monitor-crop fallback
    /// backend since process start.
    pub vision_monitor_crop_count: u64,
    /// Reason string for the most recent CapturePreview→monitor-crop fallback,
    /// or `None` if no fallback has occurred this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_last_fallback_reason: Option<String>,
    /// RFC3339 timestamp of the most recent fallback, or `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_last_fallback_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationOccurredResponse {
    /// The new mutation_id after the bump. Caller can pin this to verify
    /// subsequent captures see post-mutation state.
    pub mutation_id: u64,
}

// ============================================================================
// Capture helpers — xcap → image::RgbaImage → vision_core::Frame
// ============================================================================

/// Capture the runner window and return a vision-core [`Frame`].
///
/// On Windows, tries the occlusion-immune WebView2 `CapturePreview` path first
/// (`screenshots::capture_webview_contents`); on any error it falls back to the
/// monitor-crop path (`screenshots::capture_runner_window_crop`, the single
/// shared crop fn — formerly duplicated here as `capture_runner_window_rgba`).
///
/// Acquires a permit from `state.vision_capture_semaphore` around *whichever*
/// backend runs. xcap is GDI-bound on Windows and exhibits the "fits-2-
/// parallel-then-thrashes" pattern documented in `proj_supervisor_build_pool.md`
/// — the bounded permit pool (size 2) prevents thrash under multi-agent load;
/// it is harmless overhead for CapturePreview.
pub(super) async fn capture_runner_window_frame(state: &Arc<ApiState>) -> Result<Frame, String> {
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

    // Prefer occlusion-immune WebView2 CapturePreview on Windows.
    #[cfg(windows)]
    {
        match super::screenshots::capture_webview_contents(state).await {
            Ok(png) => match image::load_from_memory(&png) {
                Ok(img) => {
                    let scale_factor = window.scale_factor().unwrap_or(scale);
                    state
                        .vision_capture_preview_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Frame::from_rgba(
                        img.to_rgba8(),
                        FrameSource {
                            kind: qontinui_vision_core::FrameSourceKind::Window,
                            scale_factor,
                            captured_at: chrono::Utc::now(),
                            capture_backend: Some(
                                qontinui_vision_core::CaptureBackend::Webview2CapturePreview,
                            ),
                        },
                    ));
                }
                Err(e) => {
                    let reason = format!("CapturePreview PNG decode failed: {}", e);
                    record_capture_fallback(state, &reason);
                }
            },
            Err(e) => {
                let reason = format!("CapturePreview capture failed: {}", e);
                record_capture_fallback(state, &reason);
            }
        }
    }

    let (rgba, monitor_scale) = tokio::task::spawn_blocking(move || {
        super::screenshots::capture_runner_window_crop(x, y, w, h, scale)
    })
    .await
    .map_err(|e| format!("capture task join error: {}", e))??;

    state
        .vision_monitor_crop_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(Frame::from_rgba(
        rgba,
        FrameSource {
            kind: qontinui_vision_core::FrameSourceKind::Window,
            scale_factor: monitor_scale,
            captured_at: chrono::Utc::now(),
            capture_backend: Some(qontinui_vision_core::CaptureBackend::MonitorCrop),
        },
    ))
}

/// Record a CapturePreview→monitor-crop fallback: store the reason + timestamp
/// for `vision/health`, and log it. The FIRST fallback this session is promoted
/// to `info!` (so backend degradation reaches the supervisor stream without
/// enabling debug logging); subsequent fallbacks stay `warn!`.
fn record_capture_fallback(state: &Arc<ApiState>, reason: &str) {
    let now = chrono::Utc::now();
    if let Ok(mut guard) = state.vision_last_fallback.lock() {
        *guard = Some((reason.to_string(), now));
    }
    let first = !state
        .vision_capture_fallback_seen
        .swap(true, std::sync::atomic::Ordering::Relaxed);
    if first {
        info!(
            "{}; falling back to monitor-crop (first this session)",
            reason
        );
    } else {
        warn!("{}; falling back to monitor-crop", reason);
    }
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
// Cache directory (for the GET-by-sha256 streaming handler)
// ============================================================================

/// Path to the on-disk vision cache. Matches the root passed to
/// `VisionCache::new` in `mcp_api::api_state_init`. Used only by
/// `vision_cache_get_handler`, which streams files directly without
/// going through the in-memory LRU index.
fn cache_dir() -> PathBuf {
    PathBuf::from("tmp_vision_cache")
}

/// Render a cache entry's on-disk path as the host-agnostic relative form
/// `tmp_vision_cache/<sha256>.<ext>` for [`CaptureResponse::path`].
///
/// The `VisionCache` root is built **absolute** in `mcp_api::api_state_init`
/// (`current_runner_path().join("tmp_vision_cache")`) so file IO is correct
/// regardless of CWD. The response `path` is diagnostic-only — every consumer
/// fetches bytes by `sha256` via `/vision/raw` (`fetchVisionCacheBytes`), never
/// by `path` — so we strip the absolute prefix and emit the relative path the
/// doc comment and `cache_dir()` already promise. Cache entries are always flat
/// files directly under the root, so the file name plus the fixed
/// `tmp_vision_cache/` prefix is sufficient; the smoke gate's
/// `^tmp_vision_cache[\\/]` regex accepts either slash flavour.
fn relative_cache_path(p: &StdPath) -> String {
    match p.file_name() {
        Some(name) => format!("tmp_vision_cache/{}", name.to_string_lossy()),
        None => p.to_string_lossy().into_owned(),
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// `POST /ui-bridge/vision/capture` — replaces `/control/screenshot`. When
/// the request body has `captures: [...]`, dispatches to the multi-output
/// path (single xcap → N pipelines via [`qontinui_vision_core::multi_run`]);
/// otherwise produces a single output.
async fn vision_capture_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<CaptureRequest>>,
) -> Result<Json<ApiResponse<VisionCaptureResp>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut req = body.map(|b| b.0).unwrap_or_default();
    if let Some(captures) = req.captures.take() {
        let force = req.force.unwrap_or(false);
        let captures_map = do_multi_capture(&state, captures, force, &req.target).await?;
        return Ok(Json(ApiResponse::success(VisionCaptureResp::Multi {
            captures: captures_map,
        })));
    }
    let resp = do_capture(&state, req, false).await?;
    Ok(Json(ApiResponse::success(VisionCaptureResp::Single(resp))))
}

/// `POST /ui-bridge/vision/annotate` — alias for `capture` with an explicit
/// annotations array. Phase 2 rejects the `annotate_elements` selector form
/// (auto-deriving annotations from a `discover` snapshot is Phase 3+).
async fn vision_annotate_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<CaptureRequest>>,
) -> Result<Json<ApiResponse<VisionCaptureResp>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    if req.annotate_elements.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "annotate_elements selector is not implemented — \
                 pass an explicit `annotations` array instead",
            )),
        ));
    }
    if req.captures.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "vision/annotate does not support multi-output `captures` — \
                 use vision/capture for fan-out",
            )),
        ));
    }
    let resp = do_capture(&state, req, true).await?;
    Ok(Json(ApiResponse::success(VisionCaptureResp::Single(resp))))
}

async fn do_capture(
    state: &Arc<ApiState>,
    req: CaptureRequest,
    require_annotations: bool,
) -> Result<CaptureResponse, (StatusCode, Json<ApiResponse<()>>)> {
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
            return Ok(CaptureResponse {
                path: relative_cache_path(&hit.path),
                sha256: hit.sha256_hex,
                width: decoded.width(),
                height: decoded.height(),
                bytes: bytes.len(),
                format: ext.to_string(),
                contract: contract.name.to_string(),
                // Cache hit: the originating backend is not recorded with the
                // cached image, so backend provenance is unavailable here.
                capture_backend: None,
            });
        }
    }

    let provider = resolve_frame_provider(state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let capture_backend = capture_backend_label(&frame);

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

    Ok(CaptureResponse {
        path: relative_cache_path(&hit.path),
        sha256: hit.sha256_hex,
        width,
        height,
        bytes: bytes.len(),
        format: ext.to_string(),
        contract: contract.name.to_string(),
        capture_backend,
    })
}

/// Wire-string label for a frame's capture backend, for response echo.
/// `Some("Webview2CapturePreview" | "MonitorCrop")` when the frame carries a
/// runner-window backend; `None` for device / synthetic frames.
fn capture_backend_label(frame: &Frame) -> Option<String> {
    frame.source.capture_backend.map(|b| {
        serde_json::to_value(b)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", b))
    })
}

/// Multi-output capture-once fan-out (plan §3.4). For each [`NamedCapture`]:
/// compose its cache key, check the cache; build the list of misses. If any
/// misses, capture the frame ONCE (one xcap, under one permit) and run each
/// missed pipeline against a clone of the same `Frame`. Hits skip the
/// pipeline entirely. Result: a single xcap feeds N derived outputs with
/// at most one cache-write per miss.
async fn do_multi_capture(
    state: &Arc<ApiState>,
    captures: Vec<NamedCapture>,
    force: bool,
    target: &Option<String>,
) -> Result<std::collections::HashMap<String, CaptureResponse>, (StatusCode, Json<ApiResponse<()>>)>
{
    use std::collections::HashMap;
    if captures.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "vision/capture multi-output mode requires a non-empty `captures` array",
            )),
        ));
    }
    // Reject duplicate names — response is keyed by name and silent overwrites
    // are confusing.
    let mut seen = std::collections::HashSet::new();
    for cap in &captures {
        if !seen.insert(cap.name.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("duplicate capture name: {}", cap.name))),
            ));
        }
    }

    // First pass: resolve contracts + cache keys + look up hits.
    struct Pending {
        name: String,
        req: CaptureRequest,
        contract: OutputContract,
        ext: &'static str,
        cache_key: [u8; 32],
    }
    let mut results: HashMap<String, CaptureResponse> = HashMap::new();
    let mut misses: Vec<Pending> = Vec::new();
    for cap in &captures {
        let mut req = CaptureRequest::from(cap);
        // Propagate the parent request's frame source onto each derived
        // capture so its cache key namespaces by target (NamedCapture has no
        // per-capture target of its own).
        req.target = target.clone();
        let contract = resolve_contract(req.contract.as_deref())
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
        let format = pick_format(&contract).expect("contract has at least one format");
        let ext = extension_for(format);
        let cache_key = compose_capture_cache_key(state, &req);

        if !force {
            if let Some(hit) = state.vision_cache.get(&cache_key) {
                let bytes = std::fs::read(&hit.path).map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("read cached: {}", e))),
                    )
                })?;
                let decoded = image::load_from_memory(&bytes).map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("decode cached: {}", e))),
                    )
                })?;
                results.insert(
                    cap.name.clone(),
                    CaptureResponse {
                        path: relative_cache_path(&hit.path),
                        sha256: hit.sha256_hex,
                        width: decoded.width(),
                        height: decoded.height(),
                        bytes: bytes.len(),
                        format: ext.to_string(),
                        contract: contract.name.to_string(),
                        // Cache hit: originating backend not recorded.
                        capture_backend: None,
                    },
                );
                continue;
            }
        }
        misses.push(Pending {
            name: cap.name.clone(),
            req,
            contract,
            ext,
            cache_key,
        });
    }

    // All hits? Skip xcap entirely.
    if misses.is_empty() {
        debug!(
            "vision/capture multi: {} hits, 0 misses → no xcap",
            results.len()
        );
        return Ok(results);
    }

    // Capture frame once (one xcap, one permit) and run each missed pipeline
    // against a clone of the same Frame.
    let provider = resolve_frame_provider(state, target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame_w = frame.width;
    let frame_h = frame.height;
    let capture_backend = capture_backend_label(&frame);

    let mut runs: Vec<(
        String,
        qontinui_vision_core::Pipeline,
        OutputContract,
        &'static str,
        [u8; 32],
    )> = Vec::with_capacity(misses.len());
    for p in misses {
        let crop = resolve_crop_region(state, &p.req.region, &p.req.element, frame_w, frame_h)
            .await
            .map_err(|(code, msg)| (code, Json(api_error(msg))))?;
        let annotations: Vec<Annotation> = p
            .req
            .annotations
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect();
        let redact: Vec<RedactRegion> = p
            .req
            .redact
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect();
        let pipeline = build_pipeline(p.contract, crop, annotations, redact);
        runs.push((p.name, pipeline, p.contract, p.ext, p.cache_key));
    }

    let pipelines: Vec<(String, qontinui_vision_core::Pipeline)> = runs
        .iter()
        .map(|(name, pipeline, _, _, _)| (name.clone(), pipeline.clone()))
        .collect();

    let multi_results = qontinui_vision_core::multi_run(frame, pipelines);
    let miss_count = runs.len();

    for ((name, _, contract, ext, cache_key), (_name, result)) in
        runs.into_iter().zip(multi_results)
    {
        let bytes = result.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("pipeline '{}': {}", name, e))),
            )
        })?;
        let decoded = image::load_from_memory(&bytes).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("decode '{}': {}", name, e))),
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
                    Json(api_error(format!("cache put '{}': {}", name, e))),
                )
            })?;
        results.insert(
            name,
            CaptureResponse {
                path: relative_cache_path(&hit.path),
                sha256: hit.sha256_hex,
                width,
                height,
                bytes: bytes.len(),
                format: ext.to_string(),
                contract: contract.name.to_string(),
                capture_backend: capture_backend.clone(),
            },
        );
    }

    info!(
        "vision/capture multi: {} total outputs, {} cache MISS pipelines on a single xcap",
        results.len(),
        miss_count
    );
    Ok(results)
}

/// `POST /ui-bridge/vision/diff` — capture twice + naive pixel diff.
async fn vision_diff_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(req): UiBridgeJson<DiffRequest>,
) -> Result<Json<ApiResponse<DiffResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mode = req.mode.as_deref().unwrap_or("delta").to_lowercase();
    if !["delta", "overlay", "side_by_side"].contains(&mode.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("unknown diff mode '{}'", mode))),
        ));
    }

    let mut baseline_req: CaptureRequest = req.baseline.into();
    let mut comparison_req: CaptureRequest = req.comparison.into();
    // A single top-level `target` drives both captures. Threading it onto each
    // derived request makes `produce_intermediate_frame` source from the target
    // and namespaces the diff cache key (which Debug-formats both reqs).
    baseline_req.target = req.target.clone();
    comparison_req.target = req.target.clone();

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
    // Echo the comparison frame's capture backend on the diff envelope.
    let capture_backend = capture_backend_label(&comparison_frame);

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
            capture_backend: None,
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

    // Cache the diff under (mutation_id, baseline, comparison, mode).
    let mut_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    let cache_key_input = format!(
        "v=1|diff|mut={mut_id}|mode={mode}|baseline={baseline_req:?}|comparison={comparison_req:?}"
    );
    let cache_key = qontinui_vision_core::sha256_of(cache_key_input.as_bytes());
    let hit = state
        .vision_cache
        .put(&cache_key, &bytes, ext)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("cache put: {}", e))),
            )
        })?;

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
        "vision/diff: mode={} ratio={:.4} key={} bytes={}",
        mode,
        ratio,
        &hit.sha256_hex[..12],
        bytes.len()
    );

    Ok(Json(ApiResponse::success(DiffResponse {
        capture: CaptureResponse {
            path: relative_cache_path(&hit.path),
            sha256: hit.sha256_hex,
            width: decoded.width(),
            height: decoded.height(),
            bytes: bytes.len(),
            format: ext.to_string(),
            contract: contract.name.to_string(),
            capture_backend,
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

    let provider = resolve_frame_provider(state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(state)
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
        target: None,
    });
    let reason = req.reason.clone().unwrap_or_default();
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

    let provider = resolve_frame_provider(&state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let capture_backend = capture_backend_label(&frame);

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

    // Cache the raw capture under (mutation_id, request, reason). Note: raw
    // bypasses the contract entirely but still benefits from cache reuse for
    // the VGA grounding pipeline (same window state → same bytes).
    let mut_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    let cache_key_input = format!("v=1|raw|mut={mut_id}|reason={reason}|req={req:?}");
    let cache_key = qontinui_vision_core::sha256_of(cache_key_input.as_bytes());
    let hit = state
        .vision_cache
        .put(&cache_key, &bytes, "png")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("cache put: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(CaptureResponse {
        path: relative_cache_path(&hit.path),
        sha256: hit.sha256_hex,
        width: decoded.width(),
        height: decoded.height(),
        bytes: bytes.len(),
        format: "png".to_string(),
        contract: "raw".to_string(),
        capture_backend,
    })))
}

/// `POST /ui-bridge/vision/extract` (plan §3.2, Phase 4) — capture +
/// PaddleOCR-via-llama-swap → text blocks with bbox. **No pixels in
/// the response.** Cache-keyed by (mutation_id, request shape).
async fn vision_extract_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<ExtractRequest>>,
) -> Result<Json<ApiResponse<ExtractResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let force = req.force.unwrap_or(false);
    let min_conf = req.min_confidence.unwrap_or(0.5).clamp(0.0, 1.0);

    let client = OcrClient::from_env();
    let model_name = std::env::var(vision_ai::ENV_OCR_MODEL)
        .unwrap_or_else(|_| vision_ai::DEFAULT_OCR_MODEL.to_string());

    // Cache key: composed pre-capture from request shape + mutation id.
    let mut_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    let cache_input = format!(
        "v=1|extract|mut={mut_id}|model={}|min_conf={:.3}|req={req:?}",
        model_name, min_conf
    );
    let cache_key = qontinui_vision_core::sha256_of(cache_input.as_bytes());

    if !force {
        if let Some(hit) = state.vision_cache.get(&cache_key) {
            let bytes = std::fs::read(&hit.path).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("read cached extract: {}", e))),
                )
            })?;
            let mut resp: ExtractResponse = serde_json::from_slice(&bytes).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("decode cached extract: {}", e))),
                )
            })?;
            resp.cached = true;
            debug!(
                "vision/extract: cache HIT key={} blocks={}",
                &hit.sha256_hex[..12],
                resp.blocks.len()
            );
            return Ok(Json(ApiResponse::success(resp)));
        }
    }

    // Miss → capture + encode + call OCR.
    let (png_bytes, capture_backend) =
        capture_and_encode_png(&state, &req.region, &req.element, &req.target)
            .await
            .map_err(|(code, msg)| (code, Json(api_error(msg))))?;
    let (blocks, aggregate_text) = client
        .extract(&png_bytes, "image/png", min_conf)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("OCR call: {}", e))),
            )
        })?;
    let resp = ExtractResponse {
        blocks,
        aggregate_text,
        model: model_name.clone(),
        cached: false,
        capture_backend,
    };
    // Cache the response as JSON for next lookup. Strip backend provenance from
    // the cached copy — a future cache hit is not the live backend.
    let resp_json = serde_json::to_vec(&ExtractResponse {
        capture_backend: None,
        ..resp.clone()
    })
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("encode extract: {}", e))),
        )
    })?;
    if let Err(e) = state.vision_cache.put(&cache_key, &resp_json, "json") {
        warn!("vision/extract: cache put failed: {} (continuing)", e);
    }
    info!(
        "vision/extract: cache MISS model={} blocks={} aggregate_chars={}",
        model_name,
        resp.blocks.len(),
        resp.aggregate_text.chars().count()
    );
    Ok(Json(ApiResponse::success(resp)))
}

/// `POST /ui-bridge/vision/describe` (plan §3.2, Phase 4) — capture +
/// VLM caption. **No pixels in the response.** Cache-keyed by
/// (mutation_id, request shape, max_tokens, prompt).
async fn vision_describe_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<DescribeRequest>>,
) -> Result<Json<ApiResponse<DescribeResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let force = req.force.unwrap_or(false);
    let max_tokens = req.max_tokens.unwrap_or(256).clamp(64, 4096);

    let client = VlmClient::from_env();
    let model_name = std::env::var(vision_ai::ENV_VLM_MODEL)
        .unwrap_or_else(|_| vision_ai::DEFAULT_VLM_MODEL.to_string());

    let mut_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    let cache_input = format!(
        "v=1|describe|mut={mut_id}|model={}|tokens={}|req={req:?}",
        model_name, max_tokens
    );
    let cache_key = qontinui_vision_core::sha256_of(cache_input.as_bytes());

    if !force {
        if let Some(hit) = state.vision_cache.get(&cache_key) {
            let bytes = std::fs::read(&hit.path).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("read cached describe: {}", e))),
                )
            })?;
            let mut resp: DescribeResponse = serde_json::from_slice(&bytes).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("decode cached describe: {}", e))),
                )
            })?;
            resp.cached = true;
            debug!(
                "vision/describe: cache HIT key={} chars={}",
                &hit.sha256_hex[..12],
                resp.description.chars().count()
            );
            return Ok(Json(ApiResponse::success(resp)));
        }
    }

    // describe/ has no captureBackend field in its response; the backend label
    // is intentionally ignored here.
    let (png_bytes, _capture_backend) =
        capture_and_encode_png(&state, &req.region, &req.element, &req.target)
            .await
            .map_err(|(code, msg)| (code, Json(api_error(msg))))?;
    let vlm = client
        .describe(&png_bytes, "image/png", req.prompt.as_deref(), max_tokens)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("VLM call: {}", e))),
            )
        })?;
    let resp = DescribeResponse {
        description: vlm.description,
        structured: vlm.structured,
        tokens: vlm.tokens,
        model: model_name.clone(),
        cached: false,
    };
    let resp_json = serde_json::to_vec(&resp).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("encode describe: {}", e))),
        )
    })?;
    if let Err(e) = state.vision_cache.put(&cache_key, &resp_json, "json") {
        warn!("vision/describe: cache put failed: {} (continuing)", e);
    }
    info!(
        "vision/describe: cache MISS model={} chars={}",
        model_name,
        resp.description.chars().count()
    );
    Ok(Json(ApiResponse::success(resp)))
}

/// Capture the runner window, optionally crop to a region or element, and
/// encode as PNG bytes. Shared by `vision/extract` and `vision/describe` —
/// both want the same "raw-ish PNG to feed the model" output.
async fn capture_and_encode_png(
    state: &Arc<ApiState>,
    region_req: &Option<RegionRequest>,
    element_id: &Option<String>,
    target: &Option<String>,
) -> Result<(Vec<u8>, Option<String>), (StatusCode, String)> {
    let provider = resolve_frame_provider(state, target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let frame = provider
        .frame(state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let capture_backend = capture_backend_label(&frame);
    let crop =
        resolve_crop_region(state, region_req, element_id, frame.width, frame.height).await?;
    // PNG-only pipeline. No alpha policy here — we want lossless bytes to
    // feed the model; the model handles its own preprocessing.
    let mut pipeline = qontinui_vision_core::Pipeline::new();
    if let Some(region) = crop {
        pipeline = pipeline.push(Stage::CropRegion(region));
    }
    pipeline = pipeline.push(Stage::Encode(EncodedFormat::Png));
    let bytes = pipeline.run(frame).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("pipeline: {}", e),
        )
    })?;
    Ok((bytes, capture_backend))
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

/// `POST /ui-bridge/vision/mutation-occurred` — frontend signal that
/// rendered pixels have changed via a path the runner can't observe
/// directly (route change, app-driven re-render, animation settle).
/// Bumps `vision_mutation_id` so the next capture re-renders instead
/// of returning a stale cache entry. Intended caller: the SDK's
/// `window.__UI_BRIDGE__.mutationOccurred()` helper — fire-and-forget;
/// body is ignored.
async fn vision_mutation_occurred_handler(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<MutationOccurredResponse>> {
    bump_mutation_id(&state);
    let mutation_id = state
        .vision_mutation_id
        .load(std::sync::atomic::Ordering::Relaxed);
    Json(ApiResponse::success(MutationOccurredResponse {
        mutation_id,
    }))
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
    let vision_capture_preview_count = state
        .vision_capture_preview_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let vision_monitor_crop_count = state
        .vision_monitor_crop_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let (vision_last_fallback_reason, vision_last_fallback_at) = state
        .vision_last_fallback
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|(reason, at)| (Some(reason), Some(at.to_rfc3339())))
        .unwrap_or((None, None));

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
        vision_capture_preview_count,
        vision_monitor_crop_count,
        vision_last_fallback_reason,
        vision_last_fallback_at,
    }))
}

// ============================================================================
// Router + manifest
// ============================================================================

// ============================================================================
// Phase 6: vision/analyze + vision/assert + vision/baseline endpoints
// ============================================================================

/// One baseline as stored in `ApiState.vision_baselines`. Combines the
/// vision-core `BaselineEntry` (element bboxes for layout-shift checks)
/// with provenance.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineRegistryEntry {
    pub name: String,
    /// SHA-256 of the captured PNG at baseline time. Echoed in the
    /// list endpoint so callers can detect mismatches.
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub registered_at_unix_ms: i64,
    pub element_bboxes: std::collections::HashMap<String, qontinui_vision_core::Region>,
}

impl From<&BaselineRegistryEntry> for qontinui_vision_core::BaselineEntry {
    fn from(b: &BaselineRegistryEntry) -> Self {
        qontinui_vision_core::BaselineEntry {
            element_bboxes: b.element_bboxes.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    pub analyzer: qontinui_vision_core::Analyzer,
    /// Snapshot supplied by the caller. The runner does not auto-fetch
    /// from `discover` — callers (skills, tests) bring their own
    /// snapshot for deterministic input. A future revision can add an
    /// `auto_snapshot: true` flag that triggers a runner-side discover.
    #[serde(default)]
    pub snapshot: Option<qontinui_vision_core::ElementSnapshot>,
    /// Optional: name of a registered baseline to use as a prior frame
    /// for the `dynamic` analyzer.
    #[serde(default)]
    pub prior_frame_sha256: Option<String>,
    /// Optional: capture region. If `None`, captures the full window.
    #[serde(default)]
    pub region: Option<RegionRequest>,
    #[serde(default)]
    pub element: Option<String>,
    /// Optional vision target. `None` analyzes the runner's own desktop
    /// window; a device/app id sources the frame from that target instead —
    /// see [`super::vision_frame_source`].
    #[serde(default)]
    pub target: Option<String>,
}

impl RequestHints for AnalyzeRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `analyzer` (one of: \"layout\", \"typography\", \"color\", \
             \"dynamic\", \"elements\"). \
             Optional: `snapshot` (ElementSnapshot), `region`, `element`, `target`."
                .to_string(),
            "Use `vision/assert` for targeted pass/fail checks; \
             use `vision/analyze` for broad findings across a frame."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "allowedAnalyzers": ["layout", "typography", "color", "dynamic", "elements"]
        }))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResponse {
    pub analyzer: qontinui_vision_core::Analyzer,
    pub findings: Vec<qontinui_vision_core::Finding>,
    pub frame: AnalyzedFrameInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzedFrameInfo {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertRequest {
    pub assertions: Vec<qontinui_vision_core::Assertion>,
    /// Snapshot supplied by the caller.
    #[serde(default)]
    pub snapshot: Option<qontinui_vision_core::ElementSnapshot>,
    /// Optional OCR blocks the caller pre-fetched via `vision/extract`.
    /// `contains_text` assertions on regions / elements without
    /// snapshot text fall back to these.
    #[serde(default)]
    pub ocr_blocks: Option<Vec<vision_ai::OcrBlock>>,
    /// Optional vision target. `None` asserts against the runner's own
    /// desktop window; a device/app id sources the frame from that target
    /// instead — see [`super::vision_frame_source`].
    #[serde(default)]
    pub target: Option<String>,
}

impl RequestHints for AssertRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `assertions` (array of Assertion objects). \
             Each assertion has a `type` discriminator plus type-specific fields."
                .to_string(),
            "Assertion `type` values: no_overlap, contains_text, text_fits_container, \
             aligned_horizontally, aligned_vertically, color_within, typography_consistent, \
             no_layout_shift_since, no_clipping, animation_settled, contrast_meets_wcag."
                .to_string(),
            "Optional top-level fields: `snapshot` (ElementSnapshot from /discover), \
             `ocr_blocks` (from /vision/extract), `target` (device/app id)."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "allowedAssertionTypes": [
                "no_overlap",
                "contains_text",
                "text_fits_container",
                "aligned_horizontally",
                "aligned_vertically",
                "color_within",
                "typography_consistent",
                "no_layout_shift_since",
                "no_clipping",
                "animation_settled",
                "contrast_meets_wcag"
            ],
            "exampleAssertion": {
                "type": "no_overlap",
                "elements": ["element-id-a", "element-id-b"]
            }
        }))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertResponse {
    pub results: Vec<qontinui_vision_core::AssertionResult>,
    pub all_passed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineRequest {
    pub name: String,
    /// Snapshot to capture as the baseline. Caller supplies because the
    /// runner doesn't auto-discover here either.
    pub snapshot: qontinui_vision_core::ElementSnapshot,
    /// Optional: capture-spec for the baseline image. If absent, captures
    /// the full window with the PNG-strict contract.
    #[serde(default)]
    pub capture: Option<CaptureSpec>,
    /// Optional vision target. `None` baselines the runner's own desktop
    /// window; a device/app id sources the frame from that target instead —
    /// see [`super::vision_frame_source`].
    #[serde(default)]
    pub target: Option<String>,
}

impl RequestHints for BaselineRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required fields: `name` (string identifier for the baseline), \
             `snapshot` (ElementSnapshot with an `elements` array from /discover). \
             Optional: `capture` (CaptureSpec), `target` (device/app id)."
                .to_string(),
            "Capture the snapshot first via POST /ui-bridge/sdk/discover or \
             /ui-bridge/control/discover, then pass the result here."
                .to_string(),
        ])
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineCreateResponse {
    pub name: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub registered_at_unix_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineListResponse {
    pub baselines: Vec<BaselineRegistryEntry>,
}

/// `POST /ui-bridge/vision/analyze` — run a named analyzer over a
/// captured frame + caller-supplied [`ElementSnapshot`]. Returns
/// structured findings; never pixels.
async fn vision_analyze_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(req): UiBridgeJson<AnalyzeRequest>,
) -> Result<Json<ApiResponse<AnalyzeResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // `target` selects the frame source: None = runner desktop (today's
    // behavior); a device/app id sources from that target. visual-audit relies
    // on this to analyze a paired device rather than the runner window.
    let provider = resolve_frame_provider(&state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let width = frame.width;
    let height = frame.height;

    let snapshot = req.snapshot.as_ref();
    let prior = None; // future: look up by sha256 in cache

    let input = qontinui_vision_core::AnalyzeInput {
        frame: Some(&frame),
        snapshot,
        prior_frame: prior,
    };
    let findings = qontinui_vision_core::analyzers::run(req.analyzer, &input);

    info!(
        "vision/analyze: analyzer={:?} findings={}",
        req.analyzer,
        findings.len()
    );
    Ok(Json(ApiResponse::success(AnalyzeResponse {
        analyzer: req.analyzer,
        findings,
        frame: AnalyzedFrameInfo { width, height },
    })))
}

/// `POST /ui-bridge/vision/assert` — evaluate a list of declarative
/// assertions over a captured frame + caller-supplied snapshot/OCR.
/// Returns per-assertion pass/fail + reason.
async fn vision_assert_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(req): UiBridgeJson<AssertRequest>,
) -> Result<Json<ApiResponse<AssertResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // `target` selects the frame source: None = runner desktop (today's
    // behavior); a device/app id sources from that target. visual-audit relies
    // on this to assert against a paired device rather than the runner window.
    let provider = resolve_frame_provider(&state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    // Project the registry into the vision-core BaselineEntry map
    // (matches the assertion DSL's expected shape).
    let baselines_owned: std::collections::HashMap<String, qontinui_vision_core::BaselineEntry> = {
        let guard = state.vision_baselines.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error("vision_baselines mutex poisoned")),
            )
        })?;
        guard
            .iter()
            .map(|(k, v)| (k.clone(), qontinui_vision_core::BaselineEntry::from(v)))
            .collect()
    };

    let ocr_borrowed: Option<Vec<qontinui_vision_core::OcrBlockRef<'_>>> =
        req.ocr_blocks.as_ref().map(|blocks| {
            blocks
                .iter()
                .map(|b| qontinui_vision_core::OcrBlockRef {
                    bbox: qontinui_vision_core::Region {
                        x: b.bbox.x,
                        y: b.bbox.y,
                        w: b.bbox.w,
                        h: b.bbox.h,
                    },
                    text: b.text.as_str(),
                    confidence: b.confidence,
                })
                .collect()
        });

    let ctx = qontinui_vision_core::EvalContext {
        snapshot: req.snapshot.as_ref(),
        frame: Some(&frame),
        ocr_blocks: ocr_borrowed.as_deref(),
        baselines: Some(&baselines_owned),
    };

    let results: Vec<_> = req
        .assertions
        .iter()
        .map(|a| qontinui_vision_core::evaluate_assertion(a, &ctx))
        .collect();
    let all_passed = results.iter().all(|r| r.passed);

    info!(
        "vision/assert: {} assertions, {} passed, {} failed",
        results.len(),
        results.iter().filter(|r| r.passed).count(),
        results.iter().filter(|r| !r.passed).count()
    );

    Ok(Json(ApiResponse::success(AssertResponse {
        results,
        all_passed,
    })))
}

/// `POST /ui-bridge/vision/baseline` — capture a baseline image + record
/// the snapshot's bboxes under `name`. Subsequent
/// `Assertion::NoLayoutShiftSince { baseline: name }` checks compare
/// against the recorded bboxes.
async fn vision_baseline_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(req): UiBridgeJson<BaselineRequest>,
) -> Result<Json<ApiResponse<BaselineCreateResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // `target` selects the frame source: None = runner desktop (today's
    // behavior); a device/app id sources from that target so a baseline can be
    // captured from a paired device rather than the runner window.
    let provider = resolve_frame_provider(&state, &req.target)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let frame = provider
        .frame(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    // Encode for storage. Use PNG-strict so the baseline is lossless.
    let capture_req: CaptureRequest = match req.capture {
        Some(spec) => spec.into(),
        None => CaptureRequest {
            contract: Some("png_strict".into()),
            ..Default::default()
        },
    };
    let contract = resolve_contract(capture_req.contract.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    let crop = resolve_crop_region(
        &state,
        &capture_req.region,
        &capture_req.element,
        frame.width,
        frame.height,
    )
    .await
    .map_err(|(code, msg)| (code, Json(api_error(msg))))?;
    let pipeline = build_pipeline(contract, crop, Vec::new(), Vec::new());
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
    let width = decoded.width();
    let height = decoded.height();

    // Cache the image under (baseline-name).
    let cache_key =
        qontinui_vision_core::sha256_of(format!("v=1|baseline|name={}", req.name).as_bytes());
    let hit = state
        .vision_cache
        .put(&cache_key, &bytes, "png")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("cache put: {}", e))),
            )
        })?;

    let element_bboxes: std::collections::HashMap<String, qontinui_vision_core::Region> = req
        .snapshot
        .elements
        .iter()
        // `bbox` is now `Option<Region>` — only elements with a measured bbox
        // can participate in layout-shift baselines; skip bbox-less (hidden/
        // unmeasured) elements rather than failing the whole baseline.
        .filter_map(|e| e.bbox.map(|b| (e.id.clone(), b)))
        .collect();
    let registered_at_unix_ms = chrono::Utc::now().timestamp_millis();
    let entry = BaselineRegistryEntry {
        name: req.name.clone(),
        sha256: hit.sha256_hex.clone(),
        width,
        height,
        registered_at_unix_ms,
        element_bboxes,
    };

    {
        let mut guard = state.vision_baselines.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error("vision_baselines mutex poisoned")),
            )
        })?;
        guard.insert(req.name.clone(), entry);
    }

    info!(
        "vision/baseline: name={} sha={} {}x{}",
        req.name,
        &hit.sha256_hex[..12],
        width,
        height
    );

    Ok(Json(ApiResponse::success(BaselineCreateResponse {
        name: req.name,
        sha256: hit.sha256_hex,
        width,
        height,
        registered_at_unix_ms,
    })))
}

/// `GET /ui-bridge/vision/baselines` — list registered baselines for the
/// current runner instance.
async fn vision_baselines_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<BaselineListResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let guard = state.vision_baselines.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("vision_baselines mutex poisoned")),
        )
    })?;
    let baselines: Vec<_> = guard.values().cloned().collect();
    Ok(Json(ApiResponse::success(BaselineListResponse {
        baselines,
    })))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/vision/capture", post(vision_capture_handler))
        .route("/ui-bridge/vision/annotate", post(vision_annotate_handler))
        .route("/ui-bridge/vision/diff", post(vision_diff_handler))
        .route("/ui-bridge/vision/raw", post(vision_raw_handler))
        .route("/ui-bridge/vision/extract", post(vision_extract_handler))
        .route("/ui-bridge/vision/describe", post(vision_describe_handler))
        .route("/ui-bridge/vision/analyze", post(vision_analyze_handler))
        .route("/ui-bridge/vision/assert", post(vision_assert_handler))
        .route("/ui-bridge/vision/baseline", post(vision_baseline_handler))
        .route(
            "/ui-bridge/vision/baselines",
            get(vision_baselines_list_handler),
        )
        .route(
            "/ui-bridge/vision/cache/{sha256}",
            get(vision_cache_get_handler),
        )
        .route("/ui-bridge/vision/health", get(vision_health_handler))
        .route(
            "/ui-bridge/vision/mutation-occurred",
            post(vision_mutation_occurred_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/vision/capture"),
        ("POST", "/ui-bridge/vision/annotate"),
        ("POST", "/ui-bridge/vision/diff"),
        ("POST", "/ui-bridge/vision/raw"),
        ("POST", "/ui-bridge/vision/extract"),
        ("POST", "/ui-bridge/vision/describe"),
        ("POST", "/ui-bridge/vision/analyze"),
        ("POST", "/ui-bridge/vision/assert"),
        ("POST", "/ui-bridge/vision/baseline"),
        ("GET", "/ui-bridge/vision/baselines"),
        ("GET", "/ui-bridge/vision/cache/{sha256}"),
        ("GET", "/ui-bridge/vision/health"),
        ("POST", "/ui-bridge/vision/mutation-occurred"),
    ]
}
