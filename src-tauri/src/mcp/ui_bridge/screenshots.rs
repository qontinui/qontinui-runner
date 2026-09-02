//! Page-health analysis, annotations CRUD, and runner-window capture helpers.
//!
//! Family scope (post Phase 2 vision-pipeline rewrite):
//!   - `/control/page-health` — DOM-derived health summary (no pixels in
//!     response). Kept here because its helpers
//!     (`capture_runner_window`, `capture_runner_window_base64`,
//!     `lookup_element_normalized_rect`) are also reused by the new
//!     `vision_routes` module and `errors::ui_bridge_readiness_handler`.
//!   - Annotation CRUD (`/control/annotations*`, `/annotations*`).
//!
//! The native screenshot / annotated-screenshot / element-screenshot /
//! capture-element-images / diagnose-stuck-screen / media-audit handlers and
//! their wire types (`ScreenshotRequest`, `ScreenshotResponse`,
//! `AnnotatedScreenshotData`, `AnnotatedScreenshotQuery`) were deleted in
//! Phase 2 of the UI Bridge Vision Pipeline plan; consumers migrated to
//! `/ui-bridge/vision/*` (see `vision_routes.rs`).
//!
//! Notes:
//!   - `capture_runner_window_base64` is `pub` and re-exported from `mod.rs`
//!     so `errors.rs::ui_bridge_readiness_handler` can call it via
//!     `super::capture_runner_window_base64`.
//!   - `lookup_element_normalized_rect` is `pub(super)` so
//!     `vision_routes.rs` can resolve element-id → pixel-space rect through
//!     the same `discover` IPC.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
#[cfg(windows)]
use tracing::debug;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::screen;

use super::request::{ui_bridge_request_sync, wrap_ipc_result};

/// Bound on the native monitor-crop capture.
///
/// A full-screen grab of a large display is real work — hundreds of ms is
/// normal — so this is generous compared to
/// [`super::window_probe::WINDOW_GETTER_TIMEOUT`]. It exists to stop an
/// indefinite hang, not to police latency.
const NATIVE_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// ============================================================================
// Screenshot helper types
// ============================================================================

/// Internal capture payload — wraps the base64 PNG returned by
/// `capture_runner_window` and the dimensions / scale used by
/// `capture_runner_window_base64`.
///
/// Was previously the public response shape for the deleted
/// `/control/annotated-screenshot` endpoint; now an implementation detail.
#[derive(Debug)]
struct CapturedRunnerWindow {
    screenshot: String,
    width: i32,
    height: i32,
}

/// Encode a DynamicImage as base64 PNG.
fn encode_image_to_base64(image: &image::DynamicImage) -> Result<String, String> {
    use base64::Engine;
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner()))
}

/// Capture the runner's own window by cropping from a monitor screenshot.
/// xcap skips same-process windows, so we capture the monitor and crop.
///
/// Returns the cropped `RgbaImage` plus the monitor's `scale_factor`.
/// This is the single source of truth for the monitor-crop fallback path:
/// both `capture_runner_window` (base64 entry point here) and
/// `vision_routes::capture_runner_window_frame` (RGBA → `Frame`) call it,
/// so the two previously line-for-line-duplicated crop bodies are now one.
///
/// DPI handling:
/// - Tauri `outer_position()` / `outer_size()` return physical pixels.
/// - xcap `Monitor::x()` / `y()` return logical coordinates (dmPosition).
/// - xcap `Monitor::width()` / `height()` return physical pixels (dmPelsWidth/Height).
/// - The captured image is at physical resolution.
///
/// To match monitors: convert Tauri physical position to logical using scale_factor.
/// To crop the image: work in physical pixels (image coords = physical).
///
/// Runs the synchronous xcap call on the calling thread — callers must wrap in
/// `tokio::task::spawn_blocking`.
pub(super) fn capture_runner_window_crop(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
) -> Result<(image::RgbaImage, f64), String> {
    let mgr = screen::MonitorManager::detect()?;

    // Convert Tauri physical position to logical for monitor matching.
    // Use the window center so partially-off-screen windows still find the right monitor.
    let logical_x = (phys_x as f64 / scale) as i32;
    let logical_y = (phys_y as f64 / scale) as i32;
    let logical_center_x = logical_x + (phys_w as f64 / scale / 2.0) as i32;
    let logical_center_y = logical_y + (phys_h as f64 / scale / 2.0) as i32;

    let monitor = mgr
        .at_logical_point(logical_center_x, logical_center_y)
        .ok_or_else(|| "Runner window not on any monitor".to_string())?;
    let monitor_scale = monitor.scale_factor;

    let captured = screen::CapturedScreenshot::from_monitor(&mgr, monitor.index)?;

    // Convert logical window position to physical pixel offset in the captured image.
    let (rel_local_x, rel_local_y) = monitor.to_monitor_local(logical_x, logical_y);
    let (rel_phys_x, rel_phys_y) = monitor.logical_to_physical(rel_local_x, rel_local_y);

    // Handle negative offsets (window partially off-screen)
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
            "Runner window has zero visible area (crop: {}x{} at ({}, {}), image: {}x{}, scale: {})",
            crop_w,
            crop_h,
            crop_x,
            crop_y,
            captured.physical_width,
            captured.physical_height,
            monitor.scale_factor
        ));
    }

    let cropped = captured.image.crop_imm(crop_x, crop_y, crop_w, crop_h);
    Ok((cropped.to_rgba8(), monitor_scale))
}

/// Monitor-crop capture returning a base64 PNG + dims (the legacy fallback
/// shape for `capture_runner_window_base64`). Thin wrapper over the shared
/// [`capture_runner_window_crop`].
fn capture_runner_window(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
    _title: &str,
) -> Result<CapturedRunnerWindow, String> {
    let (rgba, _monitor_scale) = capture_runner_window_crop(phys_x, phys_y, phys_w, phys_h, scale)?;
    let width = rgba.width() as i32;
    let height = rgba.height() as i32;
    let b64 = encode_image_to_base64(&image::DynamicImage::ImageRgba8(rgba))?;

    Ok(CapturedRunnerWindow {
        screenshot: b64,
        width,
        height,
    })
}

/// Occlusion-immune capture of the runner's WebView2 composition surface via
/// `ICoreWebView2::CapturePreview`. Unlike the monitor-crop fallback (which
/// screenshots the physical screen and crops, so it captures whatever pixels
/// are on top — an occluding window, the desktop if minimized, etc.),
/// `CapturePreview` renders the webview's own composition surface regardless
/// of z-order, focus, or occlusion.
///
/// Returns the PNG bytes. The work is scheduled onto the WebView2 UI thread
/// via Tauri's `with_webview` (which returns immediately); the completion
/// handler — also fired on the UI thread — reads the in-memory `IStream`
/// and sends the bytes back over a `oneshot` channel that this async fn
/// awaits with a 5s timeout. We never block the UI thread.
#[cfg(windows)]
pub(super) async fn capture_webview_contents(state: &Arc<ApiState>) -> Result<Vec<u8>, String> {
    use tauri::Manager;

    // webview2-com re-exports the WebView2 COM types as `Microsoft`; they are
    // generated against `windows 0.61`, which is also what our renamed
    // `windows-capture` dep (alias `win`) provides — so `IStream` /
    // `SHCreateMemStream` here are the *same* types `CapturePreview` expects.
    use webview2_com::CapturePreviewCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
    use windows_capture::Win32::System::Com::IStream;
    use windows_capture::Win32::UI::Shell::SHCreateMemStream;

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "Runner window not found".to_string())?;

    // Physical inner size — logged against the CapturePreview dims on first
    // capture so hardware verification can confirm the surface-size contract.
    //
    // BOUNDED (see `window_probe`): `inner_size()` is a blocking event-loop
    // round-trip. Called bare from this `async fn` it parked a tokio WORKER
    // thread indefinitely whenever the UI thread was wedged — i.e. precisely
    // when a diagnostic capture is requested — so `num_cpus` concurrent
    // captures could silence the entire HTTP surface. It is diagnostic-only
    // here, so an unresponsive loop degrades to `None` and the capture
    // continues rather than aborting.
    let inner = match super::window_probe::inner_size(&window).await {
        Ok(size) => size,
        Err(e) => {
            warn!("inner_size probe failed before CapturePreview: {e}");
            None
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Vec<u8>, String>>();

    // `with_webview` schedules the closure on the UI thread and returns
    // immediately; the closure runs there (so all COM calls below, and the
    // CapturePreview completion handler, execute on the WebView2 UI thread).
    window
        .with_webview(move |wv| {
            // SAFETY: every call in this block is a COM call into WebView2 /
            // shlwapi. They run on the WebView2 UI thread (`with_webview`
            // guarantees this), the COM objects are kept alive across the
            // call by the surrounding scope, and the completion handler owns
            // the only `oneshot::Sender`. `SHCreateMemStream(None)` allocates
            // a fresh growable in-memory stream.
            let result: Result<(), String> = (|| unsafe {
                let controller = wv.controller();
                let core = controller
                    .CoreWebView2()
                    .map_err(|e| format!("CoreWebView2(): {e}"))?;

                let stream: IStream =
                    SHCreateMemStream(None).ok_or_else(|| "SHCreateMemStream returned null".to_string())?;

                // Move the stream into the completion handler so it stays
                // alive until CapturePreview finishes writing, then read it.
                let stream_for_handler = stream.clone();
                let handler = CapturePreviewCompletedHandler::create(Box::new(
                    // The handler receives the already-`.ok()`-converted result
                    // (webview2-com's `ClosureArg for HRESULT` Output is
                    // `windows::core::Result<()>`), not a raw HRESULT.
                    move |hr_result: windows_capture::core::Result<()>| -> windows_capture::core::Result<()> {
                        let payload = read_capture_stream(hr_result, &stream_for_handler);
                        // Receiver may have timed out and dropped; ignore.
                        let _ = tx.send(payload);
                        Ok(())
                    },
                ));

                core.CapturePreview(
                    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                    &stream,
                    &handler,
                )
                .map_err(|e| format!("CapturePreview: {e}"))?;
                Ok(())
            })();

            if let Err(e) = result {
                warn!("CapturePreview scheduling failed on UI thread: {}", e);
                // tx was moved into the handler only on the success path; on
                // the error path it is dropped here, which wakes the awaiter
                // with a RecvError → falls back to monitor-crop.
            }
        })
        .map_err(|e| format!("with_webview failed: {e}"))?;

    // Await the UI-thread completion handler with a timeout so a wedged UI
    // thread never hangs the caller — callers fall back to monitor-crop.
    let bytes = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(payload)) => payload?,
        Ok(Err(_recv)) => return Err("CapturePreview handler dropped sender".to_string()),
        Err(_elapsed) => return Err("CapturePreview timed out after 5s".to_string()),
    };

    // Log CapturePreview dims vs physical inner_size on success so the
    // hardware-verification step can confirm the surface-size contract.
    if let Ok(img) = image::load_from_memory(&bytes) {
        debug!(
            "CapturePreview produced {}x{} PNG ({} bytes); window inner_size = {:?}",
            img.width(),
            img.height(),
            bytes.len(),
            inner.map(|s| (s.width, s.height))
        );
    }

    Ok(bytes)
}

/// Drain a CapturePreview `IStream` into a `Vec<u8>`. Runs inside the
/// CapturePreview completion handler on the WebView2 UI thread.
#[cfg(windows)]
fn read_capture_stream(
    hr_result: windows_capture::core::Result<()>,
    stream: &windows_capture::Win32::System::Com::IStream,
) -> Result<Vec<u8>, String> {
    use windows_capture::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

    hr_result.map_err(|e| format!("CapturePreview HRESULT: {e}"))?;

    // SAFETY: COM IStream calls on a live stream owned by the caller. We Stat
    // the size, rewind to the start, then read exactly that many bytes.
    unsafe {
        let mut stat = STATSTG::default();
        stream
            .Stat(&mut stat, STATFLAG_NONAME)
            .map_err(|e| format!("IStream::Stat: {e}"))?;
        let size = stat.cbSize as usize;

        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| format!("IStream::Seek: {e}"))?;

        let mut buf = vec![0u8; size];
        let mut total_read: usize = 0;
        while total_read < size {
            let mut chunk_read: u32 = 0;
            let remaining = (size - total_read) as u32;
            let hr = stream.Read(
                buf.as_mut_ptr().add(total_read) as *mut core::ffi::c_void,
                remaining,
                Some(&mut chunk_read),
            );
            hr.ok().map_err(|e| format!("IStream::Read: {e}"))?;
            if chunk_read == 0 {
                break;
            }
            total_read += chunk_read as usize;
        }
        buf.truncate(total_read);
        Ok(buf)
    }
}

/// Capture the runner's own window as a base64 PNG string.
///
/// This is a convenience wrapper used by the snapshot and health fallback paths.
/// Returns `Some((base64_png, width, height))` on success, `None` on failure.
/// Does not require the UI Bridge SDK — uses native xcap capture.
pub async fn capture_runner_window_base64(state: &Arc<ApiState>) -> Option<(String, i32, i32)> {
    use tauri::Manager;

    // Prefer the occlusion-immune WebView2 CapturePreview path on Windows;
    // fall back to monitor-crop on any error (timeout, COM failure, etc.).
    #[cfg(windows)]
    {
        match capture_webview_contents(state).await {
            Ok(png) => {
                use base64::Engine;
                match image::load_from_memory(&png) {
                    Ok(img) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                        return Some((b64, img.width() as i32, img.height() as i32));
                    }
                    Err(e) => {
                        warn!(
                            "CapturePreview PNG decode failed: {}; falling back to monitor-crop",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "CapturePreview capture failed: {}; falling back to monitor-crop",
                    e
                );
            }
        }
    }

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())?;

    // BOUNDED (see `window_probe`): these four getters are blocking
    // event-loop round-trips, batched behind ONE timeout. Unbounded and
    // called bare from this `async fn`, they parked a tokio WORKER thread —
    // and this is the fallback arm, reached exactly when the UI thread is
    // already suspect. If the loop is wedged there is no geometry to crop
    // to, so give up on the capture instead of guessing at coordinates.
    let geometry = match super::window_probe::geometry(&window).await {
        Ok(geometry) => geometry,
        Err(e) => {
            warn!("Native capture fallback skipped — {e}");
            return None;
        }
    };
    let super::window_probe::WindowGeometry {
        scale,
        x,
        y,
        width: w,
        height: h,
        title,
    } = geometry;

    // The blocking capture itself is bounded too. Its sibling CapturePreview
    // arm has carried a 5s timeout since it was written; this arm never got
    // the same treatment, so an unbounded `spawn_blocking` join sat here.
    // Unlike the getters above this one only ever parked a blocking-pool
    // thread (an awaited JoinHandle yields its worker), so it is correctness
    // hygiene rather than the wedge fix — but an unbounded await is still an
    // unbounded await.
    let capture =
        tokio::task::spawn_blocking(move || capture_runner_window(x, y, w, h, scale, &title));
    match tokio::time::timeout(NATIVE_CAPTURE_TIMEOUT, capture).await {
        Ok(Ok(Ok(data))) => Some((data.screenshot, data.width, data.height)),
        Ok(Ok(Err(e))) => {
            warn!("Native capture fallback failed: {}", e);
            None
        }
        Ok(Err(e)) => {
            warn!("Native capture task join error: {}", e);
            None
        }
        Err(_elapsed) => {
            warn!(
                "Native capture fallback timed out after {:?}",
                NATIVE_CAPTURE_TIMEOUT
            );
            None
        }
    }
}

/// Extract a NormalizedRect from a discover element JSON value.
///
/// Tries normalizedRect first (already 0-1), then falls back to rect
/// with viewport heuristic normalization.
fn extract_normalized_rect_from_element(
    el: &serde_json::Value,
) -> Option<crate::vision::types::NormalizedRect> {
    use crate::vision::types::NormalizedRect;

    // Try normalizedRect (UI Bridge provides this directly)
    if let Some(nr) = el
        .get("normalizedRect")
        .or_else(|| el.get("state").and_then(|s| s.get("normalizedRect")))
    {
        let x = nr.get("x").and_then(|v| v.as_f64())? as f32;
        let y = nr.get("y").and_then(|v| v.as_f64())? as f32;
        let width = nr.get("width").and_then(|v| v.as_f64())? as f32;
        let height = nr.get("height").and_then(|v| v.as_f64())? as f32;
        return Some(NormalizedRect {
            x,
            y,
            width,
            height,
        });
    }

    // Fallback: use rect with absolute pixel coords
    let rect = el
        .get("rect")
        .or_else(|| el.get("state").and_then(|s| s.get("rect")))?;

    let x = rect.get("x").and_then(|v| v.as_f64())? as f32;
    let y = rect.get("y").and_then(|v| v.as_f64())? as f32;
    let w = rect.get("width").and_then(|v| v.as_f64())? as f32;
    let h = rect.get("height").and_then(|v| v.as_f64())? as f32;

    if w > 0.0 && h > 0.0 {
        // If coords look already normalized (all < 1.5), pass through
        if x < 1.5 && y < 1.5 && w < 1.5 && h < 1.5 {
            return Some(NormalizedRect {
                x,
                y,
                width: w,
                height: h,
            });
        }
        // Otherwise assume pixel coords with 1920x1080 default
        return Some(NormalizedRect {
            x: x / 1920.0,
            y: y / 1080.0,
            width: w / 1920.0,
            height: h / 1080.0,
        });
    }

    None
}

// ============================================================================
// Page Health Analysis
// ============================================================================

/// Optional request body for page-health endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageHealthRequest {
    /// Reserved for future per-check toggles.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

/// Analyse the current page by running discover internally and returning a
/// structured `PageHealthReport` with spatial coverage, layout regions,
/// element diversity, text signal scanning, interactive readiness, visual
/// anomalies and an ASCII heatmap.
pub async fn ui_bridge_page_health_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<PageHealthRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page health analysis");

    // --- Step 1: run discover to get all elements -------------------------
    let _body = body.map(|b| b.0).unwrap_or_default();

    let discover_payload = serde_json::json!({
        "options": {
            "includeHidden": true
        }
    });

    let discover_data = match ui_bridge_request_sync(&state, "discover", discover_payload).await {
        Ok(d) => d,
        Err(e) => {
            error!("UI Bridge API: page-health discover failed: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
        }
    };

    // Elements live under "elements" key returned by discover.
    let elements = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let element_count = elements.len();

    // Visible elements: state.visible == true and state.normalizedRect present.
    let visible: Vec<&serde_json::Value> = elements
        .iter()
        .filter(|el| {
            let state = el.get("state");
            let is_visible = state
                .and_then(|s| s.get("visible"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_rect = state.and_then(|s| s.get("normalizedRect")).is_some();
            is_visible && has_rect
        })
        .collect();
    let visible_count = visible.len();

    let mut findings: Vec<serde_json::Value> = Vec::new();

    // --- Step 2: Spatial coverage (20x20 grid) ----------------------------
    const GRID: usize = 20;
    let mut grid = [[false; GRID]; GRID];

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let col_start = (x * GRID as f64).floor().max(0.0) as usize;
            let col_end = ((x + w) * GRID as f64).ceil().min(GRID as f64) as usize;
            let row_start = (y * GRID as f64).floor().max(0.0) as usize;
            let row_end = ((y + h) * GRID as f64).ceil().min(GRID as f64) as usize;

            for row in grid.iter_mut().take(row_end.min(GRID)).skip(row_start) {
                for cell in row.iter_mut().take(col_end.min(GRID)).skip(col_start) {
                    *cell = true;
                }
            }
        }
    }

    let total_cells = (GRID * GRID) as f64;
    let filled_cells = grid.iter().flatten().filter(|&&v| v).count() as f64;
    let coverage_pct = (filled_cells / total_cells * 100.0).round();

    // Left half = columns 0..10, right half = columns 10..20
    let left_filled = grid
        .iter()
        .flat_map(|row| row[..GRID / 2].iter())
        .filter(|&&v| v)
        .count() as f64;
    let right_filled = grid
        .iter()
        .flat_map(|row| row[GRID / 2..].iter())
        .filter(|&&v| v)
        .count() as f64;
    let half_cells = (GRID * GRID / 2) as f64;
    let left_half_pct = (left_filled / half_cells * 100.0).round();
    let right_half_pct = (right_filled / half_cells * 100.0).round();

    let spatial_severity = if coverage_pct < 15.0 {
        "CRITICAL"
    } else if coverage_pct < 30.0 {
        "WARNING"
    } else {
        "OK"
    };

    // Sidebar-only: right < 5% and left > 20%
    let spatial_severity = if right_half_pct < 5.0 && left_half_pct > 20.0 {
        "CRITICAL"
    } else {
        spatial_severity
    };

    findings.push(serde_json::json!({
        "check": "spatial_coverage",
        "severity": spatial_severity,
        "detail": format!(
            "Elements cover {}% of viewport. Left={}%, Right={}%",
            coverage_pct, left_half_pct, right_half_pct
        ),
        "data": {
            "coverage_pct": coverage_pct,
            "left_half_pct": left_half_pct,
            "right_half_pct": right_half_pct
        }
    }));

    // --- Step 3: Layout regions -------------------------------------------
    let mut sidebar_count: usize = 0;
    let mut header_count: usize = 0;
    let mut content_count: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let cx = x + w / 2.0;
            let cy = y + h / 2.0;

            if cx < 0.2 {
                sidebar_count += 1;
            } else if cy < 0.08 {
                header_count += 1;
            } else {
                content_count += 1;
            }
        }
    }

    let layout_severity = if content_count == 0 {
        "CRITICAL"
    } else if content_count < 3 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "layout_regions",
        "severity": layout_severity,
        "detail": format!(
            "sidebar={}, header={}, content={}",
            sidebar_count, header_count, content_count
        ),
        "data": {
            "sidebar": sidebar_count,
            "header": header_count,
            "content": content_count
        }
    }));

    // --- Step 4: Element diversity ----------------------------------------
    let nav_types: &[&str] = &["button", "heading", "badge", "status-message"];
    let mut type_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for el in &elements {
        let t = el.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        *type_counts.entry(t.to_string()).or_insert(0) += 1;
    }

    let all_nav = element_count > 5 && type_counts.keys().all(|k| nav_types.contains(&k.as_str()));

    let diversity_severity = if all_nav { "WARNING" } else { "OK" };

    findings.push(serde_json::json!({
        "check": "element_diversity",
        "severity": diversity_severity,
        "detail": format!(
            "{} type(s) across {} elements{}",
            type_counts.len(),
            element_count,
            if all_nav { " (navigation-only)" } else { "" }
        ),
        "data": {
            "types": type_counts
        }
    }));

    // --- Step 5: Text signal scanning -------------------------------------
    let skip_types: &[&str] = &["button", "link", "tab", "menuitem"];

    let error_phrases: &[&str] = &[
        "error occurred",
        "failed to",
        "exception",
        "crash",
        "unavailable",
        "something went wrong",
        "could not",
    ];
    let loading_phrases: &[&str] = &[
        "loading",
        "starting",
        "connecting",
        "please wait",
        "initializing",
        "fetching",
    ];
    let empty_phrases: &[&str] = &[
        "no data",
        "no results",
        "nothing here",
        "empty",
        "no items",
        "get started",
    ];
    let css_signals: &[&str] = &["spin", "pulse", "skeleton", "loading", "shimmer"];

    let mut detected_errors: Vec<String> = Vec::new();
    let mut detected_loading: Vec<String> = Vec::new();
    let mut detected_empty: Vec<String> = Vec::new();
    let mut detected_css: Vec<String> = Vec::new();

    for el in &elements {
        let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let text = el
            .get("state")
            .and_then(|s| s.get("textContent"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // "classes" is a top-level array of strings
        let classes_arr = el.get("classes").and_then(|v| v.as_array());
        let classes_str: String = classes_arr
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        // Check CSS class signals on all elements
        let classes_lower = classes_str.to_lowercase();
        for sig in css_signals {
            if classes_lower.contains(sig) {
                detected_css.push(format!("class contains '{}' on {}", sig, el_type));
                break;
            }
        }

        // Skip navigation types for text scanning
        if skip_types.contains(&el_type) {
            continue;
        }

        let text_lower = text.to_lowercase();
        for phrase in error_phrases {
            if text_lower.contains(phrase) {
                detected_errors.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in loading_phrases {
            if text_lower.contains(phrase) {
                detected_loading.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in empty_phrases {
            if text_lower.contains(phrase) {
                detected_empty.push(text.chars().take(120).collect());
                break;
            }
        }
    }

    let text_severity = if !detected_errors.is_empty() {
        "CRITICAL"
    } else if !detected_loading.is_empty() || !detected_css.is_empty() || !detected_empty.is_empty()
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "text_signals",
        "severity": text_severity,
        "detail": format!(
            "errors={}, loading={}, empty={}, css_signals={}",
            detected_errors.len(),
            detected_loading.len(),
            detected_empty.len(),
            detected_css.len()
        ),
        "data": {
            "errors": detected_errors,
            "loading": detected_loading,
            "empty": detected_empty,
            "css_signals": detected_css
        }
    }));

    // --- Step 6: Interactive readiness ------------------------------------
    let mut interactive_total: usize = 0;
    let mut interactive_disabled: usize = 0;

    for el in &elements {
        let cat = el.get("category").and_then(|v| v.as_str()).unwrap_or("");
        if cat == "interactive" {
            interactive_total += 1;
            let enabled = el
                .get("state")
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !enabled {
                interactive_disabled += 1;
            }
        }
    }

    let interactive_severity = if interactive_total > 0
        && (interactive_disabled as f64 / interactive_total as f64) > 0.5
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "interactive_readiness",
        "severity": interactive_severity,
        "detail": format!(
            "{} interactive elements, {} disabled",
            interactive_total, interactive_disabled
        ),
        "data": {
            "total": interactive_total,
            "disabled": interactive_disabled
        }
    }));

    // --- Step 7: Visual anomalies -----------------------------------------
    let mut zero_size: usize = 0;
    let mut outside_viewport: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            if w == 0.0 || h == 0.0 {
                zero_size += 1;
            }
            if x + w < 0.0 || y + h < 0.0 || x > 1.0 || y > 1.0 {
                outside_viewport += 1;
            }
        }
    }

    let anomaly_severity = if zero_size > 0 || outside_viewport > 0 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "visual_anomalies",
        "severity": anomaly_severity,
        "detail": format!(
            "zero_size={}, outside_viewport={}",
            zero_size, outside_viewport
        ),
        "data": {
            "zero_size": zero_size,
            "outside_viewport": outside_viewport
        }
    }));

    // --- Step 8: ASCII heatmap --------------------------------------------
    let heatmap: Vec<String> = grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|&filled| if filled { '#' } else { '.' })
                .collect()
        })
        .collect();

    // --- Step 9: Determine worst severity ---------------------------------
    let severity_rank = |s: &str| -> u8 {
        match s {
            "CRITICAL" => 3,
            "WARNING" => 2,
            "OK" => 1,
            _ => 0,
        }
    };

    let worst = findings
        .iter()
        .filter_map(|f| f.get("severity").and_then(|s| s.as_str()))
        .max_by_key(|s| severity_rank(s))
        .unwrap_or("OK");

    let report = serde_json::json!({
        "summary": worst,
        "findings": findings,
        "heatmap": heatmap,
        "element_count": element_count,
        "visible_count": visible_count
    });

    Ok(Json(ApiResponse::success(report)))
}

// ============================================================================
// Annotations CRUD
// ============================================================================

/// List all annotations.
pub async fn ui_bridge_annotations_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List annotations");
    wrap_ipc_result(ui_bridge_request_sync(&state, "annotations_list", serde_json::json!({})).await)
}

/// Create annotation.
pub async fn ui_bridge_annotations_create_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Create annotation");
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "annotations_create", payload).await)
}

/// Get single annotation.
pub async fn ui_bridge_annotations_get_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    wrap_ipc_result(ui_bridge_request_sync(&state, "annotations_get", payload).await)
}

/// Update annotation.
pub async fn ui_bridge_annotations_update_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Update annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id, "updates": body } });
    wrap_ipc_result(ui_bridge_request_sync(&state, "annotations_update", payload).await)
}

/// Delete annotation.
pub async fn ui_bridge_annotations_delete_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    wrap_ipc_result(ui_bridge_request_sync(&state, "annotations_delete", payload).await)
}

/// Get annotation coverage metrics.
pub async fn ui_bridge_annotations_coverage_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Annotation coverage");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "annotations_coverage", serde_json::json!({})).await,
    )
}

/// Export annotations.
pub async fn ui_bridge_annotations_export_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Export annotations");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "annotations_export", serde_json::json!({})).await,
    )
}

pub(super) async fn lookup_element_normalized_rect(
    state: &Arc<ApiState>,
    element_id: &str,
) -> Result<Option<crate::vision::types::NormalizedRect>, String> {
    let discover_payload = serde_json::json!({ "interactive_only": false });
    let discover_data = ui_bridge_request_sync(state, "discover", discover_payload)
        .await
        .map_err(|e| format!("discover failed: {}", e))?;

    let elements_arr = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .or_else(|| {
            discover_data
                .get("data")
                .and_then(|d| d.get("elements"))
                .and_then(|v| v.as_array())
        });

    let Some(elements_arr) = elements_arr else {
        return Ok(None);
    };

    for el in elements_arr {
        let id_str = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id_str != element_id {
            continue;
        }
        return Ok(extract_normalized_rect_from_element(el));
    }

    Ok(None)
}
// ============================================================================
// Occlusion sweep - `/control/visibility`
// ============================================================================

/// Request body for `POST /ui-bridge/control/visibility`.
///
/// Matches the SDK's `visibility` handler params exactly
/// (`ui-bridge/packages/ui-bridge/src/server/handlers.ts`).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VisibilityRequest {
    /// Drop hairline overlaps below this fraction of the covered element's
    /// area. SDK default 0.02.
    #[serde(default)]
    pub min_ratio: Option<f64>,
    /// Echoed through; a tracked modal/dropdown overlay is not yet
    /// distinguishable from an accidental one on either side (see
    /// `isExpectedOverlay` below).
    #[serde(default)]
    pub include_expected: Option<bool>,
}

/// One directed occlusion relation, mirroring the SDK's
/// `VisibilityOcclusionEntry` field for field.
fn occlusion_entry(
    element_id: &str,
    label: Option<&str>,
    text: &str,
    occluded_by: &str,
    ratio: f64,
) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "element": element_id,
        "occludedBy": occluded_by,
        "ratio": ratio,
        // The SDK sets this `false` too: telling a tracked modal/dropdown
        // apart from an accidental overlay needs an overlay registry neither
        // side has. Reported honestly rather than guessed.
        "isExpectedOverlay": false,
        "hidesText": !text.is_empty(),
        // Always `hit-test`: the numbers come from the registry's
        // `elementFromPoint` sampling. The geometric arm lives in
        // ui-bridge-auto and is not consulted here.
        "source": "hit-test",
    });
    let obj = entry.as_object_mut().expect("json! built an object");
    if let Some(label) = label.filter(|l| !l.is_empty()) {
        obj.insert("label".to_string(), serde_json::json!(label));
    }
    if !text.is_empty() {
        obj.insert("text".to_string(), serde_json::json!(text));
    }
    entry
}

/// Build the `VisibilityReport` from a `discover` element array.
///
/// Split out from the handler so the whole contract is unit-testable without a
/// webview: the handler is a thin shell over one IPC call plus this.
pub(crate) fn build_visibility_report(
    elements: &[serde_json::Value],
    min_ratio: f64,
    include_expected: bool,
) -> serde_json::Value {
    let mut occlusions: Vec<(bool, f64, serde_json::Value)> = Vec::new();
    // Did ANY element carry occlusion data at all? See `occlusionDataObserved`
    // below - this is what separates "nothing is covered" from "this webview's
    // bridge cannot tell us".
    let mut any_occlusion_field = false;

    for el in elements {
        let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let state = match el.get("state") {
            Some(s) => s,
            None => continue,
        };
        if state.get("occludedBy").is_some()
            || state.get("occludedPct").is_some()
            || state.get("visibilityReason").is_some()
        {
            any_occlusion_field = true;
        }
        let occluded_by = match state.get("occludedBy").and_then(|v| v.as_str()) {
            Some(o) if !o.is_empty() => o,
            _ => continue,
        };
        // `occludedPct` is 0..100; the report's `ratio` is 0..1.
        let ratio = state
            .get("occludedPct")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            / 100.0;
        if ratio < min_ratio {
            continue;
        }
        let text = state
            .get("textContent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let label = el.get("label").and_then(|v| v.as_str());
        occlusions.push((
            !text.is_empty(),
            ratio,
            occlusion_entry(id, label, text, occluded_by, ratio),
        ));
    }

    // Worst first, and text-hiding occlusions outrank blank ones - the SDK's
    // ordering, because a covered label destroys information the reader cannot
    // recover. `total_cmp` rather than `partial_cmp().unwrap()`: a NaN ratio
    // from a malformed payload must not panic the sort.
    occlusions.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
    let entries: Vec<serde_json::Value> = occlusions.into_iter().map(|(_, _, e)| e).collect();

    let verdict = if elements.is_empty() {
        // An empty list from a registry with no elements is UNKNOWN, not
        // "nothing is covered" - the SDK models this case too.
        "unknown_empty_registry"
    } else if entries.is_empty() {
        "clear"
    } else {
        "occlusions_found"
    };

    serde_json::json!({
        "occlusions": entries,
        "elementCount": elements.len(),
        "minRatio": min_ratio,
        "includeExpected": include_expected,
        "verdict": verdict,
        // -- Runner-only advisory field, deliberately OUTSIDE the verdict --
        //
        // The verdict union stays exactly the SDK's three variants so no
        // consumer breaks on an unknown value. But a `clear` minted here is
        // only as good as the data the webview handed us, and the webview's
        // @qontinui/ui-bridge is version-pinned: occlusion sampling landed in
        // ui-bridge `4284cd2` (2026-08-26) and is absent from the 0.24.0
        // bundle this runner currently ships, whose `getElementState` emits no
        // `occludedBy` / `occludedPct` / `visibilityReason` at all.
        //
        // `false` therefore means: no element in this snapshot carried any
        // occlusion field, so `clear` is "no occlusion OBSERVED", not proof
        // that nothing is covered. Reading it as proof is the
        // silent-empty-is-unknown trap. It flips to `true` on its own once the
        // SDK pin is bumped and the page has anything occluded.
        "occlusionDataObserved": any_occlusion_field,
    })
}

/// `POST /control/visibility` - WHAT IS COVERING WHAT, page-wide.
///
/// The SDK has declared this route since ui-bridge `4284cd2` (2026-08-26) and
/// the runner did not expose it - a 404 that
/// `manifest_drift_tests::sdk_manifest_routes_are_exposed_by_runner` had been
/// failing on, which in turn MASKED every other SDK/runner drift behind the
/// same red test (manual-test-loop iteration 23, item 2).
///
/// Implemented rather than baselined, on the evidence: the SDK handler is pure
/// registry analysis with no browser-extension or DOM-only dependency (its own
/// doc calls it "sourced entirely from the registry's `elementFromPoint`
/// hit-test"), the runner already exposes its structural twin `pageHealth` the
/// same way, and the defect that motivated the SDK route was measured ON THIS
/// APP - `4284cd2`'s message cites "a floating widget covering session names on
/// the runner's Terminal page" surviving every automated check. A route whose
/// motivating bug lives in the runner's own webview cannot be dismissed as
/// meaningless there.
///
/// Shaped like `ui_bridge_page_health_handler`: one `discover` IPC for the FULL
/// element set (`includeHidden: true` AND `interactiveOnly: false`), then pure
/// analysis in Rust. Going the other way -
/// forwarding a `visibility` request type to the webview - would answer
/// "unknown request type": the frontend dispatches UI Bridge requests through
/// hand-written `use*Events` hooks, not through the SDK's own handler table.
pub async fn ui_bridge_visibility_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<VisibilityRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let min_ratio = req.min_ratio.unwrap_or(0.02);
    let include_expected = req.include_expected.unwrap_or(false);
    info!(
        "UI Bridge API: visibility sweep (minRatio={}, includeExpected={})",
        min_ratio, include_expected
    );

    // BOTH filters stated. `includeHidden` alone is only half the ask: the SDK
    // defaults `interactiveOnly` to `true`, so a payload that leaves it unstated
    // silently drops every heading, paragraph and badge before the sweep ever
    // sees the page — i.e. exactly the elements a reader looks at, and exactly
    // the ones an occlusion sweep exists to protect. Measured on this build,
    // 2/2 reps: `discover {includeHidden:true, interactiveOnly:true}` -> 281
    // elements, `interactiveOnly:false` -> 296, and this handler reported 281
    // while its structural twin `page-health` reported 296. `visibility` was
    // the sole outlier on every page tested (22/32, 206/208, 207/211,
    // 281/296). Hidden and non-interactive are INDEPENDENT filters, so asking
    // for one while leaving the other at its default under-reports every page.
    //
    // Stated as a literal rather than through a shared helper only because this
    // branch has none; `fix/terminal-path-divergences` introduces
    // `discover_all_elements_payload()` and its guard test accepts either form
    // for exactly this reason. Any sweep added here must ask the same question.
    let discover_payload = serde_json::json!({
        "options": { "includeHidden": true, "interactiveOnly": false }
    });
    let discover_data = match ui_bridge_request_sync(&state, "discover", discover_payload).await {
        Ok(d) => d,
        Err(e) => {
            error!("UI Bridge API: visibility discover failed: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
        }
    };
    let elements = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(Json(ApiResponse::success(build_visibility_report(
        &elements,
        min_ratio,
        include_expected,
    ))))
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/page-health",
            post(ui_bridge_page_health_handler),
        )
        .route(
            "/ui-bridge/control/visibility",
            post(ui_bridge_visibility_handler),
        )
        // Annotations CRUD
        .route(
            "/ui-bridge/control/annotations",
            get(ui_bridge_annotations_list_handler).post(ui_bridge_annotations_create_handler),
        )
        // SDK declares POST /control/annotation/:id as setAnnotation
        // (same action as PUT). Mounted alongside PUT so SDK callers
        // using either method hit the same update handler.
        .route(
            "/ui-bridge/control/annotation/{id}",
            get(ui_bridge_annotations_get_handler)
                .post(ui_bridge_annotations_update_handler)
                .put(ui_bridge_annotations_update_handler)
                .delete(ui_bridge_annotations_delete_handler),
        )
        .route(
            "/ui-bridge/control/annotations/coverage",
            get(ui_bridge_annotations_coverage_handler),
        )
        .route(
            "/ui-bridge/control/annotations/export",
            get(ui_bridge_annotations_export_handler),
        )
        // SDK top-level /annotations/* aliases — same handlers as the
        // /control/annotation* family above. Mounted here for symmetry with
        // the SDK contract (see UI_BRIDGE_ROUTES `path: '/annotations/...'`).
        .route(
            "/ui-bridge/annotations",
            get(ui_bridge_annotations_list_handler),
        )
        .route(
            "/ui-bridge/annotations/coverage",
            get(ui_bridge_annotations_coverage_handler),
        )
        .route(
            "/ui-bridge/annotations/export",
            get(ui_bridge_annotations_export_handler),
        )
        .route(
            "/ui-bridge/annotations/{id}",
            get(ui_bridge_annotations_get_handler)
                .put(ui_bridge_annotations_update_handler)
                .delete(ui_bridge_annotations_delete_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/page-health"),
        ("POST", "/ui-bridge/control/visibility"),
        ("GET", "/ui-bridge/control/annotations"),
        ("POST", "/ui-bridge/control/annotations"),
        ("GET", "/ui-bridge/control/annotation/{id}"),
        ("POST", "/ui-bridge/control/annotation/{id}"),
        ("PUT", "/ui-bridge/control/annotation/{id}"),
        ("DELETE", "/ui-bridge/control/annotation/{id}"),
        ("GET", "/ui-bridge/control/annotations/coverage"),
        ("GET", "/ui-bridge/control/annotations/export"),
        // SDK top-level /annotations/* aliases
        ("GET", "/ui-bridge/annotations"),
        ("GET", "/ui-bridge/annotations/coverage"),
        ("GET", "/ui-bridge/annotations/export"),
        ("GET", "/ui-bridge/annotations/{id}"),
        ("PUT", "/ui-bridge/annotations/{id}"),
        ("DELETE", "/ui-bridge/annotations/{id}"),
    ]
}

#[cfg(test)]
mod visibility_sweep_payload_tests {
    //! What `/control/visibility` ASKS the webview for, before any analysis
    //! runs (manual-test-loop iteration 25, finding F3).
    //!
    //! The occlusion report can only be as complete as the element set it was
    //! handed. `includeHidden: true` was stated and `interactiveOnly: false`
    //! was not, and because the SDK defaults that second filter to `true`, the
    //! sweep silently never saw a heading, a paragraph or a badge. Measured on
    //! this build: `visibility` reported `elementCount` 281 where `snapshot`,
    //! `elements`, `discover(all + hidden)` and `page-health` all agreed on
    //! 296 — `visibility` the sole outlier, on every page tested.
    //!
    //! Scanned from source rather than exercised through the handler because
    //! the payload is built inline at the IPC call and never returned: there is
    //! no value to assert on without a live webview. The property being pinned
    //! is nonetheless exact — BOTH filters stated, in the request this handler
    //! actually issues.

    /// The body of `ui_bridge_visibility_handler`, production source only.
    fn visibility_handler_body() -> &'static str {
        let source = include_str!("screenshots.rs");
        let start = source
            .find("pub async fn ui_bridge_visibility_handler")
            .expect("the visibility handler is in this file");
        // Ends at the next item; the handler is followed by the routes section.
        let rest = &source[start..];
        let end = rest
            .find("\n// =====")
            .expect("the routes banner follows the handler");
        &rest[..end]
    }

    #[test]
    fn visibility_discover_asks_for_hidden_elements() {
        assert!(
            visibility_handler_body().contains("\"includeHidden\": true"),
            "the occlusion sweep must see elements the SDK calls hidden - a              clipped or covered element is precisely what it hunts for"
        );
    }

    #[test]
    fn visibility_discover_asks_for_non_interactive_elements_too() {
        assert!(
            visibility_handler_body().contains("\"interactiveOnly\": false"),
            "MUST be stated explicitly: the SDK's default is `true`, so leaving              it unstated filtered every heading, paragraph and badge out of the              occlusion sweep and made `elementCount` report 281 where every              sibling route reported 296. `includeHidden` does not compensate -              hidden and non-interactive are INDEPENDENT filters."
        );
    }
}

#[cfg(test)]
mod visibility_tests {
    //! `/control/visibility` - the occlusion sweep the runner did not expose
    //! (manual-test-loop iteration 23, item 2).
    //!
    //! The whole contract is exercised through `build_visibility_report`,
    //! which is the handler minus its single `discover` IPC. Both directions
    //! are covered: an occluded page must report the DIRECTED relation, and a
    //! clean page must not invent one.
    use super::build_visibility_report;
    use serde_json::json;

    /// A `discover` element as the webview emits it.
    fn el(id: &str, label: &str, state: serde_json::Value) -> serde_json::Value {
        json!({ "id": id, "label": label, "state": state })
    }

    #[test]
    fn an_occluded_element_is_reported_with_its_occluder() {
        let elements = vec![el(
            "session-name-8",
            "Zone 8 name",
            json!({
                "occludedBy": "div.floating-widget",
                "occludedPct": 44,
                "textContent": "  Zone 8: qontinui-web  ",
            }),
        )];
        let report = build_visibility_report(&elements, 0.02, false);
        assert_eq!(report["verdict"], json!("occlusions_found"));
        assert_eq!(report["elementCount"], json!(1));
        assert_eq!(report["occlusionDataObserved"], json!(true));
        let entry = &report["occlusions"][0];
        // The relation is DIRECTED: `element` is hidden, `occludedBy` is on top.
        assert_eq!(entry["element"], json!("session-name-8"));
        assert_eq!(entry["occludedBy"], json!("div.floating-widget"));
        assert_eq!(entry["label"], json!("Zone 8 name"));
        // `occludedPct` is 0..100; `ratio` is 0..1.
        assert!((entry["ratio"].as_f64().unwrap() - 0.44).abs() < 1e-9);
        // Echoing the covered text is the point - "something is covered" is
        // not actionable, naming the string is.
        assert_eq!(entry["text"], json!("Zone 8: qontinui-web"));
        assert_eq!(entry["hidesText"], json!(true));
        assert_eq!(entry["source"], json!("hit-test"));
        assert_eq!(entry["isExpectedOverlay"], json!(false));
    }

    /// Negative control: an element the registry says nothing is covering must
    /// NOT produce an entry. A sweep that reports an occlusion for every
    /// element would pass the test above and be worthless.
    #[test]
    fn an_unoccluded_page_reports_clear_and_no_entries() {
        let elements = vec![
            el(
                "btn-1",
                "Save",
                json!({ "visibilityReason": null, "textContent": "Save" }),
            ),
            el("btn-2", "Cancel", json!({ "textContent": "Cancel" })),
        ];
        let report = build_visibility_report(&elements, 0.02, false);
        assert_eq!(report["verdict"], json!("clear"));
        assert_eq!(report["occlusions"].as_array().unwrap().len(), 0);
        assert_eq!(report["elementCount"], json!(2));
    }

    /// `minRatio` must actually filter - a hairline overlap is not a finding.
    #[test]
    fn min_ratio_drops_hairline_overlaps_and_keeps_real_ones() {
        let elements = vec![
            el(
                "hairline",
                "a",
                json!({ "occludedBy": "x", "occludedPct": 1 }),
            ),
            el("real", "b", json!({ "occludedBy": "y", "occludedPct": 60 })),
        ];
        let report = build_visibility_report(&elements, 0.02, false);
        let ids: Vec<&str> = report["occlusions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["element"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["real"]);
        // Raising the floor above the real one drops it too - proving the
        // parameter is read rather than ignored.
        let strict = build_visibility_report(&elements, 0.9, false);
        assert_eq!(strict["occlusions"].as_array().unwrap().len(), 0);
        assert_eq!(strict["verdict"], json!("clear"));
        assert_eq!(strict["minRatio"], json!(0.9));
    }

    /// Text-hiding occlusions outrank blank ones, then worst ratio first.
    #[test]
    fn occlusions_are_sorted_text_hiding_first_then_worst_ratio() {
        let elements = vec![
            el(
                "blank-90",
                "",
                json!({ "occludedBy": "x", "occludedPct": 90 }),
            ),
            el(
                "text-20",
                "",
                json!({ "occludedBy": "x", "occludedPct": 20, "textContent": "hi" }),
            ),
            el(
                "text-70",
                "",
                json!({ "occludedBy": "x", "occludedPct": 70, "textContent": "yo" }),
            ),
        ];
        let report = build_visibility_report(&elements, 0.02, false);
        let ids: Vec<&str> = report["occlusions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["element"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["text-70", "text-20", "blank-90"]);
    }

    /// An empty registry is UNKNOWN, never "nothing is covered".
    #[test]
    fn an_empty_registry_is_unknown_not_clear() {
        let report = build_visibility_report(&[], 0.02, false);
        assert_eq!(report["verdict"], json!("unknown_empty_registry"));
        assert_eq!(report["elementCount"], json!(0));
        assert_eq!(report["occlusionDataObserved"], json!(false));
    }

    /// The runner's webview SDK pin (0.24.0) predates the occlusion sweep, so
    /// its `getElementState` emits none of the three fields. `clear` is then
    /// "no occlusion OBSERVED", and `occlusionDataObserved: false` is the only
    /// thing that says so.
    #[test]
    fn a_bridge_that_cannot_report_occlusion_says_so() {
        let elements = vec![el(
            "btn-1",
            "Save",
            json!({ "visible": true, "textContent": "Save" }),
        )];
        let report = build_visibility_report(&elements, 0.02, false);
        assert_eq!(report["verdict"], json!("clear"));
        assert_eq!(report["occlusionDataObserved"], json!(false));
    }

    /// Params are echoed back so a caller can audit what the sweep actually ran with.
    #[test]
    fn the_report_echoes_the_parameters_it_ran_with() {
        let report = build_visibility_report(&[el("a", "a", json!({}))], 0.25, true);
        assert_eq!(report["minRatio"], json!(0.25));
        assert_eq!(report["includeExpected"], json!(true));
    }

    /// Malformed payloads must not panic the handler.
    #[test]
    fn malformed_elements_are_skipped_rather_than_panicking() {
        let elements = vec![
            json!({ "label": "no id" }),
            json!({ "id": "no-state" }),
            el("empty-occluder", "x", json!({ "occludedBy": "" })),
            el("no-pct", "x", json!({ "occludedBy": "y" })),
        ];
        let report = build_visibility_report(&elements, 0.02, false);
        // `no-pct` has an occluder but a 0 ratio, which is below the floor.
        assert_eq!(report["occlusions"].as_array().unwrap().len(), 0);
        assert_eq!(report["elementCount"], json!(4));
    }

    /// The route must be BOTH mounted and declared - `route_entries()` is what
    /// the SDK-drift test diffs against, and a handler mounted without an
    /// entry (or vice versa) is exactly the drift class item 2 closes.
    #[test]
    fn the_route_is_declared_in_the_manifest() {
        assert!(super::route_entries().contains(&("POST", "/ui-bridge/control/visibility")));
    }
}
