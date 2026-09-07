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
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// Request / response shapes (mirrors plan §3.2)
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionRequest {
    /// Signed origin — mirrors [`Region`]. A caller may legitimately name a
    /// rectangle whose top-left sits left of / above the frame origin (an
    /// off-viewport element bbox, a secondary monitor in a virtual desktop).
    /// Crop rejects such a region; annotate/redact clamp it to the frame.
    pub x: i32,
    pub y: i32,
    /// Unsigned extent — a negative width/height is meaningless.
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
    // BOUNDED (see `window_probe`): `scale_factor` / `inner_position` /
    // `inner_size` are blocking event-loop round-trips. Bare, from this
    // `async fn`, they parked a tokio WORKER thread whenever the UI thread
    // was wedged — and this is the vision capture path, which agents hit
    // continuously, so it was the highest-traffic route into that hang.
    let geometry = super::window_probe::geometry(&window)
        .await
        .map_err(|e| e.to_string())?;

    let scale = geometry.scale;
    let x = geometry.x;
    let y = geometry.y;
    let w = geometry.width;
    let h = geometry.height;

    let _permit = state
        .vision_capture_semaphore
        .acquire()
        .await
        .map_err(|e| format!("capture semaphore acquire: {}", e))?;

    // Prefer occlusion-immune WebView2 CapturePreview on Windows.
    #[cfg(windows)]
    {
        // Fault-injection seam (plan 2026-06-07-fleet-capture-backend-
        // telemetry.md work item 4): when `QONTINUI_VISION_FORCE_CAPTURE_FAIL`
        // is set, short-circuit the CapturePreview backend to an `Err` so the
        // monitor-crop fallback ladder runs even when CapturePreview would have
        // succeeded. This is the only way the fallback path gets exercised
        // before a user's machine does it for real (a WebView2 runtime
        // regression). House style: env-flag read at the call site, like
        // `QONTINUI_VISION_RAW`. The fallback ladder is Windows-only, so the
        // flag lives inside the `#[cfg(windows)]` block.
        let capture_result: Result<Vec<u8>, String> =
            if std::env::var("QONTINUI_VISION_FORCE_CAPTURE_FAIL").is_ok() {
                Err("forced: QONTINUI_VISION_FORCE_CAPTURE_FAIL".to_string())
            } else {
                super::screenshots::capture_webview_contents(state).await
            };
        match capture_result {
            Ok(png) => match image::load_from_memory(&png) {
                Ok(img) => {
                    // Was a second `scale_factor()` event-loop round-trip.
                    // `scale` above already came from the bounded probe in
                    // this same function microseconds earlier, so re-asking
                    // bought nothing and added one more unbounded blocking
                    // call to the hot vision path.
                    let scale_factor = scale;
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

    let (rgba, monitor_scale) = spawn_blocking_tracked(move || {
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
///
/// Thin `ApiState` adapter over [`record_capture_fallback_inner`], which holds
/// the testable decision/recording logic (the live capture path needs a real
/// webview window, so the full `capture_runner_window_frame` can't be unit-
/// tested — see the test module).
fn record_capture_fallback(state: &Arc<ApiState>, reason: &str) {
    record_capture_fallback_inner(
        &state.vision_last_fallback,
        &state.vision_capture_fallback_seen,
        reason,
    );
}

/// Decision/recording layer for a capture fallback, decoupled from `ApiState`
/// so it can be unit-tested with bare `Arc`s. Stores the reason + timestamp
/// into `last_fallback` and flips `fallback_seen` (INFO-once): the FIRST call
/// this session returns having logged at `info!`, subsequent calls at `warn!`.
/// Returns `true` iff this was the first fallback (the INFO-once edge), so
/// callers/tests can assert the flip happened exactly once.
fn record_capture_fallback_inner(
    last_fallback: &std::sync::Mutex<Option<(String, chrono::DateTime<chrono::Utc>)>>,
    fallback_seen: &std::sync::atomic::AtomicBool,
    reason: &str,
) -> bool {
    let now = chrono::Utc::now();
    if let Ok(mut guard) = last_fallback.lock() {
        *guard = Some((reason.to_string(), now));
    }
    let first = !fallback_seen.swap(true, std::sync::atomic::Ordering::Relaxed);
    if first {
        info!(
            "{}; falling back to monitor-crop (first this session)",
            reason
        );
    } else {
        warn!("{}; falling back to monitor-crop", reason);
    }
    first
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
    // A normalized rect is 0-1 by construction and clamped at 0 above, so the
    // origin is always non-negative and fits the signed `Region` field.
    Some(Region {
        x: x as i32,
        y: y as i32,
        w,
        h,
    })
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
// Cache lookup (for the GET-by-sha256 streaming handler)
// ============================================================================

/// Find the cached image file `<sha>.<ext>` directly under `dir`, returning its
/// path and extension. Cache entries are always flat files at the cache root, so
/// a single `read_dir` + prefix match suffices.
///
/// `dir` MUST be the `VisionCache`'s own absolute root
/// (`state.vision_cache.root()`). A previous version resolved a bare
/// `"tmp_vision_cache"` against the process CWD — which is NOT the runner root
/// (the Tauri exe is launched from elsewhere) — so every GET-by-sha 404'd with
/// "cache empty" even though `VisionCache::put` had written the file under the
/// absolute root `current_runner_path().join("tmp_vision_cache")`.
fn find_cache_file(dir: &StdPath, sha: &str) -> std::io::Result<Option<(PathBuf, String)>> {
    let prefix = format!("{}.", sha);
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(ext) = name.strip_prefix(&prefix) {
            return Ok(Some((entry.path(), ext.to_string())));
        }
    }
    Ok(None)
}

/// Render a cache entry's on-disk path as the host-agnostic relative form
/// `tmp_vision_cache/<sha256>.<ext>` for [`CaptureResponse::path`].
///
/// The `VisionCache` root is built **absolute** in `mcp_api::api_state_init`
/// (`current_runner_path().join("tmp_vision_cache")`) so file IO is correct
/// regardless of CWD. The response `path` is diagnostic-only — every consumer
/// fetches bytes by `sha256` via `/vision/raw` (`fetchVisionCacheBytes`), never
/// by `path` — so we strip the absolute prefix and emit the relative path the
/// response contract already promises. Cache entries are always flat
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

/// Wire-string label for a frame's source kind, for response echo.
/// `snake_case`, matching the other enums the vision responses carry
/// (`verdict.state`, `snapshotAttribution.state`). Projected rather than
/// derived from `Serialize` so the wire spelling is chosen here and not
/// inherited from a crate type that has no wire consumers of its own.
fn frame_source_kind_label(kind: qontinui_vision_core::FrameSourceKind) -> &'static str {
    // Variants spelled out rather than glob-imported: a glob would bring a
    // variant named `Region` into a module whose `Region` is the geometry
    // struct, and that shadowing is a readability trap (clippy's
    // `enum_glob_use` flags it). The match has no `_` arm on purpose — a
    // variant added upstream is then a BUILD BREAK here rather than a
    // silently mislabelled wire value.
    match kind {
        qontinui_vision_core::FrameSourceKind::Window => "window",
        qontinui_vision_core::FrameSourceKind::Region => "region",
        qontinui_vision_core::FrameSourceKind::Synthetic => "synthetic",
        qontinui_vision_core::FrameSourceKind::Device => "device",
    }
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
    // A crop must name a rectangle wholly inside the frame — `fits_in` rejects
    // both an oversized region and a negative origin.
    let Some((cx, cy, cw, ch)) = region
        .fits_in(frame.width, frame.height)
        .then(|| region.clamp_to_frame(frame.width, frame.height))
        .flatten()
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "crop region {:?} does not fit in frame {}x{}",
                region, frame.width, frame.height
            ))),
        ));
    };
    // `clamp_to_frame` narrows the (now non-negative, in-bounds) signed origin
    // to the u32 indices `crop_imm` requires.
    let dyn_img = image::DynamicImage::ImageRgba8(frame.buffer);
    let cropped = dyn_img.crop_imm(cx, cy, cw, ch);
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
            // Buffer indices — always non-negative, so the cast into the
            // signed `Region` origin is lossless.
            x: min_x as i32,
            y: min_y as i32,
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
    State(state): State<Arc<ApiState>>,
    Path(sha): Path<String>,
) -> Result<Response, (StatusCode, Json<ApiResponse<()>>)> {
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) || sha.len() != 64 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("invalid sha256 (must be 64 hex chars)")),
        ));
    }
    // Serve from the SAME absolute root the writer (`VisionCache::put`) uses, via
    // the shared cache's own accessor — never a CWD-relative guess (the process
    // CWD is not the runner root, which is what 404'd every GET with "cache empty").
    let dir = state.vision_cache.root();
    if !dir.exists() {
        return Err((StatusCode::NOT_FOUND, Json(api_error("cache empty"))));
    }
    let (path, ext) = find_cache_file(dir, &sha)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("readdir: {}", e))),
            )
        })?
        .ok_or_else(|| {
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
            // `None` = an UNATTRIBUTED baseline, which is exactly what this
            // registry holds: `BaselineRegistryEntry` records no snapshot id,
            // so there is nothing to carry over. qontinui-schemas documents
            // `None` as the supported value for "a baseline written from an
            // unattributed snapshot, and every baseline file written before
            // this field existed", and `eval_layout_shift` reports those as
            // unattributed rather than silently blank.
            //
            // STOPGAP, deliberately minimal. qontinui-schemas#145 added this
            // field and landed ahead of its declared adaptation
            // (qontinui-runner#1150, `coord:upstream-of=qontinui-runner#1150`),
            // which is blocked on an unrelated frontend lockfile mismatch. That
            // left every runner CI run red on E0063 here. #1150 does the real
            // thing -- it threads a `snapshot_id` onto `BaselineRegistryEntry`
            // and carries it verbatim -- and SUPERSEDES this line when it
            // rebases. Do not build on `None` being correct long-term.
            snapshot_id: None,
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
    /// The analyzer's own verdict on whether its preconditions were met.
    ///
    /// **Read this before `findings`.** An empty `findings` list is not
    /// self-describing: under `{"state":"checked"}` it means the page is
    /// clean, while under `{"state":"blocked"}` it means the snapshot was
    /// too impoverished to check anything and the list answers nothing.
    /// Those two used to be byte-identical on the wire, which is the defect
    /// this field closes — a snapshot whose elements carry no geometry made
    /// `layout` return `[]`, indistinguishable from a genuine pass.
    pub verdict: qontinui_vision_core::AnalyzerVerdict,
    /// What the analyzer actually had to work with, computed from the
    /// snapshot itself rather than from any producer's claim.
    ///
    /// **`None` here has TWO causes and they are different facts.** Either
    /// the analyzer takes no snapshot at all (`dynamic` never sets it, even
    /// when the caller supplied one), or no snapshot was supplied.
    /// `snapshotAttribution.state` is the disambiguator: `absent` means the
    /// caller sent none, anything else means the analyzer declined to count
    /// one it was given. Read the two together — neither alone answers the
    /// question, and this field on its own was the collapsed reading before
    /// `snapshotAttribution` existed.
    ///
    /// Note also that this presence rule is NOT the assert path's: there,
    /// `coverage` is present exactly when a snapshot was supplied, because
    /// the assert handler counts unconditionally. Same function, different
    /// presence rule.
    ///
    /// Note `withStacking` counts POPULATED stacking ranks and asserts
    /// nothing about whether the producer resolved them correctly, so a high
    /// value is not by itself a trust signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<qontinui_vision_core::SnapshotCoverage>,
    /// `None` when no frame could be captured. The snapshot-only analyzers
    /// (layout, typography, elements) still ran; see `frame_error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<AnalyzedFrameInfo>,
    /// Why frame capture failed, when it did. Always reported rather than
    /// swallowed — a caller must be able to tell "the pixels agreed" from
    /// "there were no pixels".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_error: Option<String>,
    /// When this analyzer finished, on the runner's clock. Stamped the
    /// instant [`qontinui_vision_core::analyzers::run`] returns.
    ///
    /// Distinct from `frame.capturedAt`, and it is the only time that
    /// describes the OBSERVATION. `capturedAt` is present on essentially
    /// every response — the handler captures a frame best-effort whichever
    /// analyzer was asked for — but on the three snapshot-only analyzers
    /// (layout, typography, elements) it dates a frame that did not
    /// participate in the analysis at all. Present is not the same as
    /// relevant, and only this field is both.
    ///
    /// Never absent, and deliberately so — including under a
    /// [`qontinui_vision_core::AnalyzerVerdict::Blocked`] verdict. A refusal
    /// to answer is still an observation, and it is still an observation
    /// made at a particular moment; dropping the time there would make the
    /// one response a reader most needs to age the least ageable.
    pub evaluated_at: chrono::DateTime<chrono::Utc>,
    /// Identity of the snapshot this analysis consumed. See
    /// [`SnapshotAttribution`] for why this is three states rather than an
    /// `Option<String>`.
    ///
    /// Named `snapshotAttribution` rather than `snapshot` so it cannot be
    /// confused with the REQUEST's `snapshot` field, which is an
    /// `ElementSnapshot` and a completely different shape.
    pub snapshot_attribution: SnapshotAttribution,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzedFrameInfo {
    pub width: u32,
    pub height: u32,
    /// When the capture backend produced this frame, on its own clock.
    ///
    /// **Read what this DOES and DOES NOT date.** It dates the FRAME, and
    /// only the frame. It does not date the caller's snapshot, and so it
    /// does not answer "was the input to this observation stale?" — the
    /// runner captures the frame ITSELF, immediately before the analysis
    /// whose completion stamps [`AnalyzeResponse::evaluated_at`], so the gap
    /// between the two measures capture plus analysis (tens to hundreds of
    /// ms for `color` over a large frame) and never how old the caller's
    /// snapshot is. The three snapshot-only
    /// analyzers (layout, typography, elements) never read the frame at all,
    /// so on those paths this timestamp describes a resource that did not
    /// participate in the observation.
    ///
    /// Dating the INPUT would need a producer-minted capture time on
    /// [`qontinui_vision_core::ElementSnapshot`], which carries `elements`
    /// and `snapshot_id` and no timestamp. Until it does, a stale snapshot
    /// posted to a live runner is still undetectable from this response, and
    /// this field must not be read as evidence that it is not.
    ///
    /// Never absent. Every [`FrameSource`] the runner constructs stamps it,
    /// so a frame that exists has a capture time; a frame that does not
    /// exist is reported as an absent `frame` plus a `frameError`.
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// Device pixel ratio of the capture: `1.0` unscaled, `2.0` Retina. The
    /// snapshot's geometry is in CSS pixels while the frame's is in device
    /// pixels, so a consumer comparing the two needs this number and had no
    /// way to obtain it from this response.
    pub scale_factor: f64,
    /// Where the frame came from — the runner's own window, a region of it,
    /// a synthetic buffer, or an external device/app.
    ///
    /// Spelled `snake_case` on the wire, matching the two other enums these
    /// responses carry (`verdict.state`, `snapshotAttribution.state`) rather
    /// than the Rust variant names. `captureBackend` below is PascalCase
    /// instead, and deliberately so: that spelling already ships on
    /// `CaptureResponse.captureBackend` and changing it would break a
    /// consumer. This field has no such precedent — this is the first route
    /// to put `FrameSourceKind` on a wire at all — so it takes the local
    /// convention while it still can.
    pub kind: &'static str,
    /// Which capture backend produced a runner-window frame, in the same
    /// wire spelling the capture routes use (`Webview2CapturePreview` or
    /// `MonitorCrop`).
    ///
    /// **`None` here is a STATEMENT, not a gap.** The rule is exact:
    /// `Some` iff `kind == "window"`. A runner-window frame always names the
    /// backend that produced it; every other `kind` has no runner-window
    /// backend to name, so the field is empty by construction rather than
    /// unrecorded ([`qontinui_vision_core::FrameSource::capture_backend`]
    /// specifies exactly that). Note what does and does not protect that
    /// `iff`: stating it as a rule over `kind` keeps the DOC readable, but a
    /// future variant that is itself a runner-window path would break the
    /// rule, and prose cannot notice. What actually catches that is
    /// [`frame_source_kind_label`]'s exhaustive match with no `_` arm — a
    /// new variant fails the build and forces this doc to be reconsidered.
    ///
    /// One caveat this doc owes an older consumer: because the field is
    /// omitted rather than sent as `null`, its absence is byte-identical to
    /// what a runner build predating this change returns. The non-optional
    /// `snapshotAttribution` on the enclosing response is the build marker —
    /// if that key is present, the build is new and this omission is the
    /// statement above; if it is not, the whole response predates it and
    /// says nothing either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_backend: Option<String>,
}

impl AnalyzeResponse {
    /// Build the response from the analyzer's result and the inputs it ran
    /// over, deriving every projected field in ONE place.
    ///
    /// The handler used to assemble this literally, and that is how the
    /// `FrameSource` fields were lost: a struct literal at a call site can
    /// drop a projection without anything failing, and the only test that
    /// could have caught it would have had to build the same literal by
    /// hand. Naming the projection gives it a seam a test can hold — see
    /// [`AssertResponse::of`], which exists for the same reason.
    fn of(
        analyzer: qontinui_vision_core::Analyzer,
        result: qontinui_vision_core::AnalyzerResult,
        frame: Option<&Frame>,
        frame_error: Option<String>,
        snapshot: Option<&qontinui_vision_core::ElementSnapshot>,
        evaluated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            analyzer,
            findings: result.findings,
            verdict: result.verdict,
            coverage: result.coverage,
            frame: frame.map(AnalyzedFrameInfo::of),
            frame_error,
            evaluated_at,
            snapshot_attribution: SnapshotAttribution::of(snapshot),
        }
    }
}

impl AnalyzedFrameInfo {
    /// Project a captured [`Frame`] onto the wire, carrying its
    /// [`FrameSource`] provenance rather than narrowing it away.
    ///
    /// This projection used to be `{width, height}` written inline at the
    /// response-construction site, which silently discarded `captured_at`,
    /// `scale_factor`, `kind` and `capture_backend` — information the frame
    /// was already holding. Naming the projection puts the widening in one
    /// place, so the next field added to `FrameSource` has a single site to
    /// reach the wire through.
    fn of(frame: &Frame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            captured_at: frame.source.captured_at,
            scale_factor: frame.source.scale_factor,
            kind: frame_source_kind_label(frame.source.kind),
            capture_backend: capture_backend_label(frame),
        }
    }
}

/// What a vision response can say about the identity of the snapshot it was
/// handed.
///
/// Three states, kept apart deliberately. Collapsing them into one
/// `Option<String>` — the obvious shape — would make "no snapshot was
/// supplied" byte-identical to "a snapshot was supplied and carried no id",
/// and those call for different action: the first is a caller that asked for
/// a frame-only analysis, the second is a producer that mints no id yet.
/// [`qontinui_vision_core::ElementSnapshot::snapshot_id`] names the
/// unattributed case "a first-class state ... not a defect", which it can
/// only stay if the wire can express it.
///
/// Serialized like [`qontinui_vision_core::AnalyzerVerdict`]: an internally
/// tagged `state` plus the payload the state carries.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SnapshotAttribution {
    /// A snapshot was supplied and carried a producer-minted id. The token
    /// is opaque and echoed verbatim — the runner neither parses nor
    /// validates it.
    Attributed {
        #[serde(rename = "snapshotId")]
        snapshot_id: String,
    },
    /// A snapshot was supplied and carried NO id. The analysis is legitimate
    /// and simply cannot be attributed to a capture — expected today, since
    /// no producer mints the id yet.
    Unattributed,
    /// No snapshot was supplied at all. Any snapshot-derived field in this
    /// response (`coverage` above all) is absent for that reason and for no
    /// other.
    Absent,
}

impl SnapshotAttribution {
    fn of(snapshot: Option<&qontinui_vision_core::ElementSnapshot>) -> Self {
        match snapshot {
            None => Self::Absent,
            Some(s) => match &s.snapshot_id {
                Some(id) => Self::Attributed {
                    snapshot_id: id.clone(),
                },
                None => Self::Unattributed,
            },
        }
    }
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
            "Assertion `type` values: no_overlap, element_above, contains_text, \
             text_fits_container, aligned_horizontally, aligned_vertically, color_within, \
             typography_consistent, no_layout_shift_since, no_clipping, animation_settled, \
             contrast_meets_wcag."
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
                "element_above",
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
    /// Why frame capture failed, when it did. Every assertion in the DSL is
    /// evaluated from the snapshot, so this is informational — but it must
    /// be visible, not inferred from a missing field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_error: Option<String>,
    /// Provenance of the frame this call captured, when it captured one.
    ///
    /// No assertion in the DSL reads the frame — every one evaluates from
    /// the snapshot, the OCR blocks or the baseline registry — so this
    /// describes a resource that did not feed any verdict here. It is
    /// reported anyway, because the handler pays the full capture cost on
    /// every call and reporting neither the result nor the provenance is
    /// strictly worse than either alternative: a caller cannot otherwise
    /// tell what the runner was looking at when it answered.
    ///
    /// `None` states that no frame was captured, and `frameError` says why.
    /// Same build-marker caveat as `coverage` above: the key is omitted
    /// rather than `null`, so read `snapshotAttribution` first to know
    /// whether the omission is a statement or an older build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<AnalyzedFrameInfo>,
    /// What the evaluator actually had to work with, computed from the
    /// snapshot itself rather than from any producer's claim — the same
    /// [`qontinui_vision_core::SnapshotCoverage::of`] the analyze path uses,
    /// over the snapshot this handler was already handed.
    ///
    /// **Same function, different PRESENCE rule.** Here `coverage` is
    /// present exactly when a snapshot was supplied, because this handler
    /// counts unconditionally. On [`AnalyzeResponse`] it can be absent even
    /// with a snapshot in hand, because the analyzer decides. Do not read a
    /// missing `coverage` on one as meaning what it means on the other.
    ///
    /// An assert-only consumer previously got no answer at all to "what did
    /// this cover", while the analyze path had carried one since
    /// `SnapshotCoverage` landed. Every assertion in the DSL evaluates from
    /// the snapshot, so a `passed: true` over an impoverished snapshot is
    /// exactly as vacuous here as an empty finding list is there.
    ///
    /// **`None` is a STATEMENT, not a gap**: no snapshot was supplied, so
    /// there was nothing to count — `snapshotAttribution.state == "absent"`
    /// says the same thing from the other side. It never means "counting was
    /// skipped" or "the count was unavailable"; the pass is pure,
    /// O(elements) and cannot fail once a snapshot exists.
    ///
    /// N6 caveat, shared with `frame` below and with
    /// `frame.captureBackend`: the field is OMITTED rather than sent as
    /// `null`, so its absence is byte-identical to what a runner build
    /// predating this change returns. The non-optional `snapshotAttribution`
    /// is the build marker — if that key is present the build is new and
    /// this omission is the statement above; if it is not, the response
    /// predates the change and says nothing either way.
    ///
    /// Note `withStacking` counts POPULATED stacking ranks and asserts
    /// nothing about whether the producer resolved them correctly, so a high
    /// value is not by itself a trust signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<qontinui_vision_core::SnapshotCoverage>,
    /// When these assertions finished evaluating, on the runner's clock.
    ///
    /// No assertion in the DSL reads the frame — every one evaluates from
    /// the snapshot, the OCR blocks or the baseline registry — so while
    /// `frame.capturedAt` is usually present beside this, it dates a
    /// resource no verdict here consulted. This is the only time that dates
    /// the ANSWER, and it is never absent. A failing gate line that cannot
    /// be aged is a failing gate line a reviewer cannot separate from a
    /// stale one.
    pub evaluated_at: chrono::DateTime<chrono::Utc>,
    /// Identity of the snapshot these assertions were evaluated against. See
    /// [`SnapshotAttribution`] for why this is three states rather than an
    /// `Option<String>`.
    ///
    /// Named `snapshotAttribution` rather than `snapshot` so it cannot be
    /// confused with the REQUEST's `snapshot` field, which is an
    /// `ElementSnapshot` and a completely different shape.
    pub snapshot_attribution: SnapshotAttribution,
}

impl AssertResponse {
    /// Build the response from the evaluated results and the snapshot they
    /// were evaluated against, deriving every snapshot-provenance field in
    /// ONE place.
    ///
    /// The handler used to assemble this literally, which is how a
    /// projection loses a field without anything failing — the same silent
    /// narrowing that discarded `FrameSource` at the analyze construction
    /// site. Naming the projection gives it a seam a test can hold.
    fn of(
        results: Vec<qontinui_vision_core::AssertionResult>,
        snapshot: Option<&qontinui_vision_core::ElementSnapshot>,
        frame: Option<&Frame>,
        frame_error: Option<String>,
        evaluated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            all_passed: results.iter().all(|r| r.passed),
            results,
            frame_error,
            frame: frame.map(AnalyzedFrameInfo::of),
            // The same function the analyze path calls, over the snapshot
            // this handler already holds: pure, O(elements), and it cannot
            // fail. A second implementation here would be free to drift from
            // that one, which is the defect `coverage.rs`'s own module doc
            // was written about.
            coverage: snapshot.map(qontinui_vision_core::SnapshotCoverage::of),
            evaluated_at,
            snapshot_attribution: SnapshotAttribution::of(snapshot),
        }
    }
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
    //
    // Frame capture is BEST-EFFORT and must not gate the analysis.
    //
    // Three of the five analyzers — layout, typography, elements — are pure
    // geometry over the caller's snapshot and never read a pixel, and
    // `analyzers::run` already degrades the other two to an explicit
    // "skipped" finding when `frame` is `None`. Capturing first and `?`-ing
    // on failure therefore threw away every frameless analysis for a
    // resource none of them needed: with the Tauri window absent
    // (`frontendState: "window_missing"`, a headless or crashed UI) the
    // occlusion and overlap checks 500'd with "Runner window not found"
    // rather than answering from the snapshot in hand.
    let frame_result = match resolve_frame_provider(&state, &req.target).await {
        Ok(provider) => provider.frame(&state).await,
        Err(e) => Err(e),
    };
    let (frame, frame_error) = match frame_result {
        Ok(f) => (Some(f), None),
        Err(e) => {
            warn!("vision/analyze: frame capture failed, continuing snapshot-only: {e}");
            (None, Some(e))
        }
    };

    let snapshot = req.snapshot.as_ref();
    let prior = None; // future: look up by sha256 in cache

    let input = qontinui_vision_core::AnalyzeInput {
        frame: frame.as_ref(),
        snapshot,
        prior_frame: prior,
    };
    let result = qontinui_vision_core::analyzers::run(req.analyzer, &input);
    // Stamped the instant the analyzer returns, not at response assembly —
    // `evaluatedAt` says when the observation was made, and for a heavy
    // analyzer over a large frame those are measurably different moments.
    let evaluated_at = chrono::Utc::now();

    info!(
        "vision/analyze: analyzer={:?} findings={} verdict={:?} frame={}",
        req.analyzer,
        result.findings.len(),
        result.verdict,
        if frame.is_some() {
            "captured"
        } else {
            "absent"
        }
    );
    Ok(Json(ApiResponse::success(AnalyzeResponse::of(
        req.analyzer,
        result,
        frame.as_ref(),
        frame_error,
        snapshot,
        evaluated_at,
    ))))
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
    // Best-effort, for the same reason as `vision/analyze` above — and more
    // strongly here: NO assertion in the DSL reads `ctx.frame`. Every one of
    // them evaluates from the snapshot, the OCR blocks or the baseline
    // registry. Capturing a frame was a hard precondition for a value the
    // evaluator never consulted, which made `no_overlap` and `no_clipping`
    // unavailable on a headless runner for no reason at all.
    let frame_result = match resolve_frame_provider(&state, &req.target).await {
        Ok(provider) => provider.frame(&state).await,
        Err(e) => Err(e),
    };
    let (frame, frame_error) = match frame_result {
        Ok(f) => (Some(f), None),
        Err(e) => {
            warn!("vision/assert: frame capture failed, continuing snapshot-only: {e}");
            (None, Some(e))
        }
    };

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
                        // OCR blocks are detected INSIDE a frame, so their
                        // origin is a buffer index and can never be negative —
                        // `OcrBbox` stays u32 and the widening cast is lossless.
                        x: b.bbox.x as i32,
                        y: b.bbox.y as i32,
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
        frame: frame.as_ref(),
        ocr_blocks: ocr_borrowed.as_deref(),
        baselines: Some(&baselines_owned),
    };

    let results: Vec<_> = req
        .assertions
        .iter()
        .map(|a| qontinui_vision_core::evaluate_assertion(a, &ctx))
        .collect();
    // See the analyze handler: stamped when evaluation finishes, not at
    // response assembly.
    let evaluated_at = chrono::Utc::now();
    info!(
        "vision/assert: {} assertions, {} passed, {} failed",
        results.len(),
        results.iter().filter(|r| r.passed).count(),
        results.iter().filter(|r| !r.passed).count()
    );

    Ok(Json(ApiResponse::success(AssertResponse::of(
        results,
        req.snapshot.as_ref(),
        frame.as_ref(),
        frame_error,
        evaluated_at,
    ))))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The `/vision/assert` 422 hint is the only place the assertion DSL's
    /// vocabulary is advertised to a caller, and it is a hand-maintained
    /// duplicate of `qontinui_vision_core::assertions::Assertion` rather than
    /// a derivation of it — the enum has no `strum`-style variant iterator, so
    /// nothing links the two. Adding `element_above` (U3) took the list from
    /// 11 to 12; without a pin, a variant added upstream stays undiscoverable
    /// while `evaluate_assertion` (which is variant-agnostic) handles it fine.
    ///
    /// The prose hint and the structured array are ALSO two separate copies of
    /// the same list, so both are checked here.
    #[test]
    fn assert_request_hint_advertises_the_full_assertion_vocabulary() {
        const EXPECTED: [&str; 12] = [
            "no_overlap",
            "element_above",
            "contains_text",
            "text_fits_container",
            "aligned_horizontally",
            "aligned_vertically",
            "color_within",
            "typography_consistent",
            "no_layout_shift_since",
            "no_clipping",
            "animation_settled",
            "contrast_meets_wcag",
        ];

        let data = <AssertRequest as RequestHints>::shape_error_data()
            .expect("AssertRequest must advertise shape_error_data");
        let advertised: Vec<&str> = data["allowedAssertionTypes"]
            .as_array()
            .expect("allowedAssertionTypes must be an array")
            .iter()
            .map(|t| t.as_str().expect("each allowed type is a string"))
            .collect();
        assert_eq!(
            advertised,
            EXPECTED.as_slice(),
            "allowedAssertionTypes drifted from the vision-core DSL"
        );

        let prose = <AssertRequest as RequestHints>::shape_error_suggestions()
            .expect("AssertRequest must advertise shape_error_suggestions")
            .join(" ");
        for wire_name in EXPECTED {
            assert!(
                prose.contains(wire_name),
                "the prose hint omits `{wire_name}`: {prose}"
            );
        }
    }

    /// The vocabulary above is only honest if the crate this runner actually
    /// compiles against can parse it. `qontinui-vision-core` is a sibling PATH
    /// dependency, so a stale checkout would advertise `element_above` and then
    /// 422 on it.
    #[test]
    fn element_above_assertion_parses_against_the_linked_vision_core() {
        let req: AssertRequest = serde_json::from_str(
            r#"{"assertions":[
                {"type":"element_above","elements":["title-bar-dropdown","prompts-panel"]},
                {"type":"element_above","elements":["a","b"],"require_overlap":false}
            ]}"#,
        )
        .expect("element_above must parse against the linked vision-core");
        assert_eq!(req.assertions.len(), 2);
        match &req.assertions[0] {
            qontinui_vision_core::Assertion::ElementAbove {
                elements,
                require_overlap,
            } => {
                assert_eq!(elements[0], "title-bar-dropdown");
                assert_eq!(elements[1], "prompts-panel");
                assert!(*require_overlap, "require_overlap must default to true");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        match &req.assertions[1] {
            qontinui_vision_core::Assertion::ElementAbove {
                require_overlap, ..
            } => assert!(!require_overlap),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// R4: the cache-HIT branch of `vision_capture_handler` (the early
    /// `return Ok(CaptureResponse { ... capture_backend: None })`) leaves the
    /// originating backend unknown — the cached image carries no provenance.
    /// This asserts (a) a cache-hit-shaped `CaptureResponse` has
    /// `capture_backend == None`, and (b) the serialized JSON OMITS the
    /// `captureBackend` key entirely (the field is
    /// `skip_serializing_if = "Option::is_none"`), so SDK clients never see a
    /// `null`/dangling backend field on a cache hit.
    #[test]
    fn cache_hit_capture_response_omits_capture_backend() {
        // Mirror exactly the struct the cache-HIT branch builds: every field
        // populated from the cache entry, `capture_backend: None`.
        let resp = CaptureResponse {
            path: "tmp_vision_cache/deadbeef.jpeg".to_string(),
            sha256: "deadbeef".to_string(),
            width: 1280,
            height: 720,
            bytes: 4096,
            format: "jpeg".to_string(),
            contract: "claude_vision_v1".to_string(),
            // Cache hit: backend provenance is unavailable.
            capture_backend: None,
        };

        // (a) The cache-hit branch sets capture_backend to None.
        assert!(
            resp.capture_backend.is_none(),
            "cache-hit CaptureResponse must have capture_backend == None"
        );

        // (b) Serialized output must omit captureBackend (serde skip).
        let v = serde_json::to_value(&resp).expect("serialize CaptureResponse");
        let obj = v
            .as_object()
            .expect("CaptureResponse serializes to a JSON object");
        assert!(
            !obj.contains_key("captureBackend"),
            "captureBackend key must be omitted on a cache hit (skip_serializing_if), got: {v}"
        );

        // Sanity: a populated backend DOES serialize the camelCase key, proving
        // the omission above is the skip-on-None path, not a rename typo.
        let with_backend = CaptureResponse {
            capture_backend: Some("MonitorCrop".to_string()),
            ..resp
        };
        let v2 = serde_json::to_value(&with_backend).expect("serialize");
        assert_eq!(
            v2.get("captureBackend").and_then(|b| b.as_str()),
            Some("MonitorCrop"),
            "captureBackend must serialize as camelCase when Some"
        );
    }

    /// Regression: the GET-by-sha handler must scan the SAME absolute root the
    /// writer (`VisionCache::put`) uses (`VisionCache::root()`), not a
    /// CWD-relative `"tmp_vision_cache"`. Before the fix, the reader resolved a
    /// bare relative path against the process CWD (≠ runner root), so every
    /// GET-by-sha 404'd with "cache empty" even though the file existed under the
    /// absolute cache root. `find_cache_file` driven by `cache.root()` finds it.
    #[test]
    fn cache_file_found_under_vision_cache_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache = qontinui_vision_core::VisionCache::new(tmp.path(), 8 * 1024 * 1024)
            .expect("create cache");
        // The cache root is absolute — the property the reader now relies on to
        // stay independent of the process CWD.
        assert!(
            cache.root().is_absolute(),
            "VisionCache root must be absolute so the reader is CWD-independent"
        );
        let key = [0x11u8; 32];
        let hit = cache.put(&key, b"fake-jpeg-bytes", "jpeg").expect("put");

        // Driven by the cache's own root, the handler's lookup finds the file.
        let found = find_cache_file(cache.root(), &hit.sha256_hex)
            .expect("read_dir")
            .expect("file written by put() must be found under root()");
        assert_eq!(found.1, "jpeg", "extension round-trips");
        assert_eq!(
            std::fs::read(&found.0).expect("read cached file"),
            b"fake-jpeg-bytes",
            "bytes round-trip"
        );

        // An unknown sha misses cleanly (Ok(None), not an error) — the branch
        // that yields the handler's "no cache entry" 404 rather than "cache empty".
        let miss = find_cache_file(cache.root(), &"0".repeat(64)).expect("read_dir");
        assert!(miss.is_none(), "unknown sha misses cleanly");
    }

    /// Phase 1 of the vision-provenance plan. Until this, the frame
    /// projection at the analyze construction site was `{width, height}`
    /// written inline, and `FrameSource`'s `captured_at` / `scale_factor` /
    /// `kind` / `capture_backend` were discarded silently — nothing failed
    /// when the projection dropped a field, which is exactly how the capture
    /// time was lost in the first place.
    ///
    /// This pins the WIRE KEYS, not the struct: a field renamed or dropped in
    /// serialization is the failure mode, and only a serialize round-trip
    /// sees it.
    #[test]
    fn analyzed_frame_info_carries_frame_source_provenance_to_the_wire() {
        let captured_at = chrono::Utc::now();
        let frame = Frame::from_rgba(
            RgbaImage::new(4, 3),
            FrameSource {
                kind: qontinui_vision_core::FrameSourceKind::Window,
                scale_factor: 2.0,
                captured_at,
                capture_backend: Some(qontinui_vision_core::CaptureBackend::MonitorCrop),
            },
        );

        let v = serde_json::to_value(AnalyzedFrameInfo::of(&frame)).expect("serialize");

        assert_eq!(v["width"], 4);
        assert_eq!(v["height"], 3);
        assert_eq!(v["scaleFactor"], 2.0);
        assert_eq!(v["kind"], "window");
        assert_eq!(v["captureBackend"], "MonitorCrop");
        let wire_captured_at = v["capturedAt"].as_str().expect("capturedAt is a string");
        let parsed = chrono::DateTime::parse_from_rfc3339(wire_captured_at)
            .expect("capturedAt must reach the wire as parseable RFC3339")
            .with_timezone(&chrono::Utc);
        assert_eq!(
            parsed, captured_at,
            "capturedAt must be the frame's own capture time, not narrowed away or restamped"
        );
    }

    /// `captureBackend: None` is a STATEMENT — "this frame did not come from
    /// the runner's own desktop window" — and `kind` is what makes it
    /// readable as one. The pairing is the whole reason the absent value is
    /// distinguishable from an unrecorded one, so it is pinned rather than
    /// left to the doc comment alone.
    #[test]
    fn device_frame_omits_capture_backend_and_says_so_through_kind() {
        let frame = Frame::from_rgba(
            RgbaImage::new(1, 1),
            FrameSource {
                kind: qontinui_vision_core::FrameSourceKind::Device,
                scale_factor: 1.0,
                captured_at: chrono::Utc::now(),
                capture_backend: None,
            },
        );

        let v = serde_json::to_value(AnalyzedFrameInfo::of(&frame)).expect("serialize");

        assert!(
            v.get("captureBackend").is_none(),
            "a device frame must OMIT captureBackend rather than emit null"
        );
        assert_eq!(
            v["kind"], "device",
            "kind must remain present, because it is what makes the omission readable"
        );
    }

    /// The three snapshot-identity states must stay distinguishable on the
    /// wire. Collapsing them into one `Option<String>` would make "no
    /// snapshot supplied" byte-identical to "snapshot supplied, no id" —
    /// the same collapsed-distinction defect `AnalyzerVerdict` exists to
    /// prevent one level up.
    #[test]
    fn snapshot_attribution_keeps_absent_unattributed_and_attributed_apart() {
        let attributed = qontinui_vision_core::ElementSnapshot {
            snapshot_id: Some("ubs2_abc".to_string()),
            ..Default::default()
        };
        let unattributed = qontinui_vision_core::ElementSnapshot::default();

        let absent = serde_json::to_value(SnapshotAttribution::of(None)).expect("serialize");
        let unattr =
            serde_json::to_value(SnapshotAttribution::of(Some(&unattributed))).expect("serialize");
        let attr =
            serde_json::to_value(SnapshotAttribution::of(Some(&attributed))).expect("serialize");

        assert_eq!(absent["state"], "absent");
        assert_eq!(unattr["state"], "unattributed");
        assert_eq!(attr["state"], "attributed");
        assert_eq!(attr["snapshotId"], "ubs2_abc");

        assert_ne!(
            absent, unattr,
            "\"no snapshot supplied\" and \"snapshot supplied without an id\" \
             must not serialize identically"
        );
        assert!(
            unattr.get("snapshotId").is_none(),
            "an unattributed snapshot must carry no snapshotId key at all"
        );
    }

    /// A refusal to answer is still an observation, and it is still an
    /// observation made at a particular moment. `evaluatedAt` is therefore
    /// non-optional and must survive a `Blocked` verdict — the response a
    /// reader most needs to age is the one that reached no conclusion.
    #[test]
    fn analyze_response_carries_evaluated_at_even_when_blocked() {
        // Through the real projection, and with a snapshot that IS supplied,
        // so a handler that stopped reading `req.snapshot` would fail here.
        // Building the struct literally would only have pinned serde.
        let snapshot = qontinui_vision_core::ElementSnapshot {
            snapshot_id: Some("ubs2_blocked".to_string()),
            ..Default::default()
        };
        let result = qontinui_vision_core::AnalyzerResult::blocked(
            "no element carries a bbox (0/7)".to_string(),
            None,
            vec![],
        );

        let v = serde_json::to_value(AnalyzeResponse::of(
            qontinui_vision_core::Analyzer::Layout,
            result,
            None,
            Some("Runner window not found".to_string()),
            Some(&snapshot),
            chrono::Utc::now(),
        ))
        .expect("serialize");

        assert_eq!(v["verdict"]["state"], "blocked");
        assert!(
            v["evaluatedAt"].is_string(),
            "a Blocked verdict must still carry evaluatedAt: {v}"
        );
        assert_eq!(
            v["snapshotAttribution"]["snapshotId"], "ubs2_blocked",
            "the caller's snapshot id must reach the wire even under a Blocked verdict"
        );
        assert!(
            v.get("frame").is_none(),
            "no frame was captured, so `frame` must be omitted; frameError says why"
        );
        assert_eq!(v["frameError"], "Runner window not found");
    }

    /// The analyze handler's frame projection must be the NAMED one. A
    /// literal at the call site is how `FrameSource` was discarded in the
    /// first place, so this pins that a captured frame reaches the wire with
    /// its provenance rather than as `{width, height}`.
    #[test]
    fn analyze_response_carries_frame_provenance_when_a_frame_was_captured() {
        let frame = Frame::from_rgba(
            RgbaImage::new(8, 6),
            FrameSource {
                kind: qontinui_vision_core::FrameSourceKind::Window,
                scale_factor: 1.5,
                captured_at: chrono::Utc::now(),
                capture_backend: Some(qontinui_vision_core::CaptureBackend::Webview2CapturePreview),
            },
        );

        let v = serde_json::to_value(AnalyzeResponse::of(
            qontinui_vision_core::Analyzer::Color,
            qontinui_vision_core::AnalyzerResult::blocked("n/a".to_string(), None, vec![]),
            Some(&frame),
            None,
            None,
            chrono::Utc::now(),
        ))
        .expect("serialize");

        assert_eq!(v["frame"]["width"], 8);
        assert_eq!(v["frame"]["scaleFactor"], 1.5);
        assert_eq!(v["frame"]["kind"], "window");
        assert_eq!(v["frame"]["captureBackend"], "Webview2CapturePreview");
        assert!(v["frame"]["capturedAt"].is_string());
        assert_eq!(v["snapshotAttribution"]["state"], "absent");
    }

    /// Phase 2 of the vision-provenance plan. The assert path holds the same
    /// snapshot the analyze path counts, and counting it is a pure O(n) pass
    /// — an assert-only consumer had no answer at all to "what did this
    /// cover".
    ///
    /// Both arms are pinned, because the informative half of `coverage` is
    /// that its absence is a statement: `None` means no snapshot was
    /// supplied, never "the count was skipped".
    #[test]
    fn assert_response_coverage_is_present_with_a_snapshot_and_absent_without() {
        let snapshot = qontinui_vision_core::ElementSnapshot {
            elements: vec![qontinui_vision_core::Element {
                id: "a".to_string(),
                bbox: Some(Region {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let with_snapshot = serde_json::to_value(AssertResponse::of(
            vec![],
            Some(&snapshot),
            None,
            None,
            chrono::Utc::now(),
        ))
        .expect("serialize");

        assert_eq!(with_snapshot["coverage"]["elements"], 1);
        assert_eq!(with_snapshot["coverage"]["withGeometry"], 1);
        assert!(with_snapshot["evaluatedAt"].is_string());
        // NOTE: `allPassed` is `true` over an EMPTY assertion list, because
        // `.all()` on an empty iterator is vacuously true. Pre-existing
        // behaviour, unchanged here — but it is a vacuous pass on the one
        // path that has no `AnalyzerVerdict` to qualify it, and it is
        // asserted so the next reader sees it rather than rediscovering it
        // from a green gate.
        assert_eq!(with_snapshot["allPassed"], true);
        assert_eq!(
            with_snapshot["snapshotAttribution"]["state"],
            "unattributed"
        );

        let without_snapshot = serde_json::to_value(AssertResponse::of(
            vec![],
            None,
            None,
            None,
            chrono::Utc::now(),
        ))
        .expect("serialize");

        assert!(
            without_snapshot.get("coverage").is_none(),
            "absent coverage must be OMITTED, and readable as \"no snapshot was supplied\" \
             through snapshotAttribution.state"
        );
        assert_eq!(without_snapshot["snapshotAttribution"]["state"], "absent");
    }

    /// The assert handler pays the full frame-capture cost on every call and
    /// used to report nothing about it but `frameError`. Reporting neither
    /// the result nor the provenance is strictly worse than either
    /// alternative, so the provenance is carried — through the same named
    /// projection the analyze path uses.
    #[test]
    fn assert_response_carries_frame_provenance_through_the_same_projection() {
        let frame = Frame::from_rgba(
            RgbaImage::new(2, 2),
            FrameSource {
                kind: qontinui_vision_core::FrameSourceKind::Device,
                scale_factor: 1.0,
                captured_at: chrono::Utc::now(),
                capture_backend: None,
            },
        );

        let v = serde_json::to_value(AssertResponse::of(
            vec![],
            None,
            Some(&frame),
            None,
            chrono::Utc::now(),
        ))
        .expect("serialize");

        assert_eq!(v["frame"]["kind"], "device");
        assert!(v["frame"]["capturedAt"].is_string());
        assert!(
            v["frame"].get("captureBackend").is_none(),
            "a device frame names no runner-window backend, on either route"
        );
    }
}

// ============================================================================
// Capture-backend fallback ladder tests
// (plan 2026-06-07-fleet-capture-backend-telemetry.md work item 4).
//
// Windows-gated: the CapturePreview→monitor-crop fallback ladder is itself
// `#[cfg(windows)]`. The *full* `capture_runner_window_frame` path can't be
// unit-tested because it needs a live Tauri webview window (the live path was
// proven on 2026-06-06). Per the plan, we instead extract and unit-test the
// decision/recording layer the fault-injection seam exercises:
// `record_capture_fallback_inner` (last-fallback record + INFO-once flip) plus
// the monitor-crop counter bump that the real path performs at the end of
// `capture_runner_window_frame`. This is the shape that makes the fallback
// ladder's bookkeeping red-before/green-after without a window.
// ============================================================================
#[cfg(all(test, windows))]
mod capture_fallback_tests {
    use super::record_capture_fallback_inner;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Two forced fallbacks (the `QONTINUI_VISION_FORCE_CAPTURE_FAIL` seam
    /// makes every capture fail): each produces a monitor-crop frame, so the
    /// crop counter bumps once per call; the last-fallback record is populated;
    /// and the INFO-once flag flips exactly ONCE across the two captures
    /// (`record_capture_fallback_inner` returns `true` only on the first).
    ///
    /// This mirrors the bump/record/INFO-once that `capture_runner_window_frame`
    /// performs on the fallback path: `record_capture_fallback` (now delegating
    /// to `..._inner`) at the CapturePreview-failure site, then the
    /// `vision_monitor_crop_count.fetch_add(1)` after the crop succeeds.
    #[test]
    fn forced_fallback_bumps_crop_records_and_flips_info_once() {
        let crop_count = AtomicU64::new(0);
        let fallback_seen = AtomicBool::new(false);
        let last_fallback: Mutex<Option<(String, chrono::DateTime<chrono::Utc>)>> =
            Mutex::new(None);

        // Capture #1 — forced failure → fallback frame produced (MonitorCrop).
        let first_flip = record_capture_fallback_inner(
            &last_fallback,
            &fallback_seen,
            "forced: QONTINUI_VISION_FORCE_CAPTURE_FAIL",
        );
        // The real path bumps the monitor-crop counter when the crop frame is
        // produced (capture_runner_window_frame end).
        crop_count.fetch_add(1, Ordering::Relaxed);

        assert!(
            first_flip,
            "first forced fallback must be the INFO-once edge"
        );
        assert_eq!(
            crop_count.load(Ordering::Relaxed),
            1,
            "monitor_crop_count must bump to 1 after the first fallback frame"
        );
        {
            let g = last_fallback.lock().unwrap();
            let (reason, _at) = g.as_ref().expect("last_fallback must be populated");
            assert!(
                reason.contains("QONTINUI_VISION_FORCE_CAPTURE_FAIL"),
                "recorded reason must reflect the forced failure, got: {reason}"
            );
        }
        assert!(
            fallback_seen.load(Ordering::Relaxed),
            "fallback_seen flag must be flipped after the first fallback"
        );

        // Capture #2 — forced failure again → another fallback frame, but the
        // INFO-once edge must NOT re-flip (subsequent fallbacks log at warn!).
        let second_flip = record_capture_fallback_inner(
            &last_fallback,
            &fallback_seen,
            "forced: QONTINUI_VISION_FORCE_CAPTURE_FAIL",
        );
        crop_count.fetch_add(1, Ordering::Relaxed);

        assert!(
            !second_flip,
            "second fallback must NOT re-trip the INFO-once edge"
        );
        assert_eq!(
            crop_count.load(Ordering::Relaxed),
            2,
            "monitor_crop_count must bump to 2 after the second fallback frame"
        );
    }
}
