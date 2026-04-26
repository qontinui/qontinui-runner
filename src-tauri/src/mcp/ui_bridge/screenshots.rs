//! Screenshot, annotation overlay, stuck-screen diagnosis, page-health,
//! annotations CRUD, and media routes.
//!
//! Family scope:
//!   - Native xcap screenshot capture (monitor / window / runner-own-window)
//!   - Annotated screenshot overlay via `vision::annotator`
//!   - `/control/diagnose-stuck` and `/control/page-health` diagnostics
//!   - Element image capture (`capture-element-images`, `get-element-images`,
//!     `element-screenshot` single-element wrapper)
//!   - Annotation CRUD (`/control/annotations*`)
//!   - Media routes (`/ai/media/*`)
//!
//! Notes:
//!   - `capture_runner_window_base64` is `pub` and re-exported from `mod.rs`
//!     so `errors.rs::ui_bridge_readiness_handler` can call it via
//!     `super::capture_runner_window_base64` after the extraction.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tauri::Emitter;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::screen;

use super::request::{ui_bridge_request_sync, wrap_ipc_result};

// ============================================================================
// Screenshot types + helpers
// ============================================================================

/// Query parameters for annotated screenshot
#[derive(Debug, Deserialize)]
pub struct AnnotatedScreenshotQuery {
    /// Monitor index (0-based), None for primary monitor. Used for full-screen capture.
    #[serde(default)]
    monitor: Option<i32>,
    /// Capture a specific window by title (case-insensitive substring match)
    #[serde(default)]
    window_title: Option<String>,
    /// Capture a specific window by app name (case-insensitive substring match)
    #[serde(default)]
    app_name: Option<String>,
    /// Capture a specific window by its ID (HWND as u32)
    #[serde(default)]
    window_id: Option<u32>,
    /// Capture the runner's own window
    #[serde(default)]
    runner: Option<bool>,
    /// When true, overlay numbered element bounding boxes on the screenshot.
    #[serde(default)]
    annotate: Option<bool>,
    /// Maximum image width after resize (only used when annotate=true).
    /// Default: 1024. Set to 0 to skip resize.
    #[serde(default)]
    max_width: Option<u32>,
}

/// Annotated screenshot response
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotatedScreenshotData {
    screenshot: String,
    width: i32,
    height: i32,
    /// Device pixel ratio (physical pixels / CSS pixels).
    /// Use this to scale CSS element bounds to screenshot pixel coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    scale_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<u32>,
    /// Text index of annotated elements (only present when annotate=true).
    /// Format: `[1] "Login" button at (0.12, 0.34) size (0.08, 0.04)`
    #[serde(skip_serializing_if = "Option::is_none")]
    element_index_text: Option<String>,
}

impl AnnotatedScreenshotData {
    /// Create from a `CapturedScreenshot` — width/height always come from the
    /// decoded image dimensions, preventing the DPI double-multiply bug.
    fn from_captured(captured: &screen::CapturedScreenshot) -> Result<Self, String> {
        Ok(Self {
            screenshot: captured.to_png_base64()?,
            width: captured.physical_width as i32,
            height: captured.physical_height as i32,
            scale_factor: Some(captured.scale_factor),
            monitor: captured.monitor_index.map(|i| i as i32),
            window_title: None,
            window_app_name: None,
            window_id: None,
            element_index_text: None,
        })
    }
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

/// Capture a specific window by matching criteria.
fn capture_window_screenshot(
    window_title: Option<String>,
    app_name: Option<String>,
    window_id: Option<u32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;

    let target = if let Some(id) = window_id {
        windows
            .iter()
            .find(|w| w.id().unwrap_or(0) == id)
            .ok_or_else(|| format!("No window found with id {}", id))?
    } else if let Some(ref title_query) = window_title {
        let query_lower = title_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.title()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let t = w.title().unwrap_or_default();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    })
                    .take(10)
                    .collect();
                format!(
                    "No window found matching title '{}'. Available: {:?}",
                    title_query, available
                )
            })?
    } else if let Some(ref app_query) = app_name {
        let query_lower = app_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.app_name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let a = w.app_name().unwrap_or_default();
                        if a.is_empty() {
                            None
                        } else {
                            Some(a)
                        }
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .take(10)
                    .collect();
                format!(
                    "No window found matching app_name '{}'. Available: {:?}",
                    app_query, available
                )
            })?
    } else {
        return Err("No window selection criteria provided".to_string());
    };

    let title = target.title().unwrap_or_default();
    let app = target.app_name().unwrap_or_default();
    let id = target.id().unwrap_or(0);

    if target.is_minimized().unwrap_or(false) {
        return Err(format!(
            "Window '{}' ({}) is minimized — cannot capture",
            title, app
        ));
    }

    let image = target
        .capture_image()
        .map_err(|e| format!("Failed to capture window '{}': {}", title, e))?;

    // Determine the scale factor from the monitor the window is on
    let scale = {
        let win_x = target.x().unwrap_or(0);
        let win_y = target.y().unwrap_or(0);
        screen::MonitorManager::detect()
            .ok()
            .and_then(|mgr| mgr.at_logical_point(win_x, win_y).map(|m| m.scale_factor))
            .unwrap_or(1.0)
    };

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title),
        window_app_name: Some(app),
        window_id: Some(id),
        element_index_text: None,
    })
}

/// Capture the runner's own window by cropping from a monitor screenshot.
/// xcap skips same-process windows, so we capture the monitor and crop.
///
/// DPI handling:
/// - Tauri `outer_position()` / `outer_size()` return physical pixels.
/// - xcap `Monitor::x()` / `y()` return logical coordinates (dmPosition).
/// - xcap `Monitor::width()` / `height()` return physical pixels (dmPelsWidth/Height).
/// - The captured image is at physical resolution.
///
/// To match monitors: convert Tauri physical position to logical using scale_factor.
/// To crop the image: work in physical pixels (image coords = physical).
fn capture_runner_window(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
    title: &str,
) -> Result<AnnotatedScreenshotData, String> {
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
    let b64 = encode_image_to_base64(&cropped)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width: crop_w as i32,
        height: crop_h as i32,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title.to_string()),
        window_app_name: Some("Qontinui Runner".to_string()),
        window_id: None,
        element_index_text: None,
    })
}

/// Capture the runner's own window as a base64 PNG string.
///
/// This is a convenience wrapper used by the snapshot and health fallback paths.
/// Returns `Some((base64_png, width, height))` on success, `None` on failure.
/// Does not require the UI Bridge SDK — uses native xcap capture.
pub async fn capture_runner_window_base64(state: &Arc<ApiState>) -> Option<(String, i32, i32)> {
    use tauri::Manager;

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = window.inner_position().unwrap_or_default();
    let size = window.inner_size().unwrap_or_default();
    let x = pos.x;
    let y = pos.y;
    let w = size.width;
    let h = size.height;
    let title = window
        .title()
        .unwrap_or_else(|_| "Qontinui Runner".to_string());

    match tokio::task::spawn_blocking(move || capture_runner_window(x, y, w, h, scale, &title))
        .await
    {
        Ok(Ok(data)) => Some((data.screenshot, data.width, data.height)),
        Ok(Err(e)) => {
            warn!("Native capture fallback failed: {}", e);
            None
        }
        Err(e) => {
            warn!("Native capture task join error: {}", e);
            None
        }
    }
}

/// Capture a full monitor screenshot.
fn capture_monitor_screenshot(
    monitor_index: Option<i32>,
) -> Result<AnnotatedScreenshotData, String> {
    if let Some(idx) = monitor_index {
        if idx < 0 {
            return Err(format!("Monitor index must be non-negative, got {}", idx));
        }
    }

    let mgr = screen::MonitorManager::detect()?;
    let idx = monitor_index
        .map(|i| i as usize)
        .unwrap_or_else(|| mgr.primary_index());
    let captured = screen::CapturedScreenshot::from_monitor(&mgr, idx)?;
    let mut data = AnnotatedScreenshotData::from_captured(&captured)?;
    data.monitor = monitor_index;
    Ok(data)
}

/// GET /ui-bridge/control/annotated-screenshot — Screenshot with metadata
///
/// Captures natively via xcap (Rust). No Python executor dependency.
///
/// Query params (all optional, first match wins):
/// - `runner=true` — capture the runner's own Tauri window
/// - `window_title=...` — case-insensitive substring match on window title
/// - `app_name=...` — case-insensitive substring match on app name
/// - `window_id=N` — exact window ID (HWND)
/// - `monitor=N` — full monitor capture (0-based index, default: primary)
/// - (none) — captures primary monitor
pub async fn ui_bridge_annotated_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AnnotatedScreenshotQuery>,
) -> Json<ApiResponse<AnnotatedScreenshotData>> {
    let want_annotate = query.annotate.unwrap_or(false);
    let max_width = query.max_width.unwrap_or(1024);

    let is_window_capture = query.runner.unwrap_or(false)
        || query.window_title.is_some()
        || query.app_name.is_some()
        || query.window_id.is_some();

    // --- Phase 1: Capture the raw screenshot ---
    let capture_result: Result<AnnotatedScreenshotData, String> = if is_window_capture {
        info!(
            runner = ?query.runner,
            window_title = ?query.window_title,
            app_name = ?query.app_name,
            window_id = ?query.window_id,
            "UI Bridge API: Capturing window screenshot (native)"
        );

        // For runner's own window, xcap skips same-process windows,
        // so we capture the monitor and crop to the window bounds.
        if query.runner.unwrap_or(false) {
            use tauri::Manager;
            let window = state
                .app_handle
                .get_webview_window(qontinui_runner_lib::get_main_window_label());
            if let Some(win) = window {
                let scale = win.scale_factor().unwrap_or(1.0);
                // Use inner_position/inner_size for the content area (viewport).
                // Element bounds from the UI Bridge SDK are relative to the viewport,
                // not the outer window frame (which includes title bar).
                let pos = win.inner_position().unwrap_or_default();
                let size = win.inner_size().unwrap_or_default();
                let x = pos.x;
                let y = pos.y;
                let w = size.width;
                let h = size.height;
                let title = win
                    .title()
                    .unwrap_or_else(|_| "Qontinui Runner".to_string());

                match tokio::task::spawn_blocking(move || {
                    capture_runner_window(x, y, w, h, scale, &title)
                })
                .await
                {
                    Ok(Ok(data)) => {
                        info!(
                            "UI Bridge screenshot: Captured runner window ({}x{})",
                            data.width, data.height
                        );
                        Ok(data)
                    }
                    Ok(Err(e)) => {
                        error!("UI Bridge screenshot: Runner capture failed: {}", e);
                        Err(format!("Runner screenshot failed: {}", e))
                    }
                    Err(e) => {
                        error!("UI Bridge screenshot: Task join error: {}", e);
                        Err(format!("Screenshot capture task failed: {}", e))
                    }
                }
            } else {
                Err("Runner window not found".to_string())
            }
        } else {
            let window_title = query.window_title;
            let app_name = query.app_name;
            let window_id = query.window_id;

            match tokio::task::spawn_blocking(move || {
                capture_window_screenshot(window_title, app_name, window_id)
            })
            .await
            {
                Ok(Ok(data)) => {
                    info!(
                        "UI Bridge screenshot: Captured window '{}' ({}x{}, id={})",
                        data.window_title.as_deref().unwrap_or("?"),
                        data.width,
                        data.height,
                        data.window_id.unwrap_or(0),
                    );
                    Ok(data)
                }
                Ok(Err(e)) => {
                    error!("UI Bridge screenshot: Window capture failed: {}", e);
                    Err(format!("Window screenshot failed: {}", e))
                }
                Err(e) => {
                    error!("UI Bridge screenshot: Task join error: {}", e);
                    Err(format!("Screenshot capture task failed: {}", e))
                }
            }
        }
    } else {
        // Full monitor capture (existing behavior)
        info!(
            monitor = ?query.monitor,
            "UI Bridge API: Capturing monitor screenshot (native)"
        );

        let monitor = query.monitor;
        match tokio::task::spawn_blocking(move || capture_monitor_screenshot(monitor)).await {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured {}x{} from monitor {:?}",
                    data.width, data.height, data.monitor
                );
                Ok(data)
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Monitor capture failed: {}", e);
                Err(format!("Screenshot capture failed: {}", e))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Err(format!("Screenshot capture task failed: {}", e))
            }
        }
    };

    // --- Phase 2: If annotate=true, overlay element bounding boxes ---
    let mut data = match capture_result {
        Ok(d) => d,
        Err(e) => return Json(ApiResponse::error(e)),
    };

    if want_annotate {
        match apply_annotation(&state, &mut data, max_width).await {
            Ok(()) => {
                info!(
                    "UI Bridge screenshot: Annotation applied (max_width={})",
                    max_width
                );
            }
            Err(e) => {
                warn!(
                    "UI Bridge screenshot: Annotation failed, returning raw screenshot: {}",
                    e
                );
                // Fall through — return the unannotated screenshot
            }
        }
    }

    Json(ApiResponse::success(data))
}

/// Apply element annotation overlay to a captured screenshot.
///
/// Calls UI Bridge discover to get elements, converts them to `AnnotatedElement`,
/// and runs the annotation engine to overlay numbered bounding boxes.
async fn apply_annotation(
    state: &Arc<ApiState>,
    data: &mut AnnotatedScreenshotData,
    max_width: u32,
) -> Result<(), String> {
    use crate::vision::annotator::{annotate_screenshot, AnnotatedElement};

    // Discover elements from the UI Bridge SDK
    let discover_payload = serde_json::json!({ "interactive_only": false });
    let discover_data = ui_bridge_request_sync(state, "discover", discover_payload)
        .await
        .map_err(|e| format!("Discover failed: {}", e))?;

    // Extract elements array — may be at top-level or nested under "data"
    let elements_arr = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .or_else(|| {
            discover_data
                .get("data")
                .and_then(|d| d.get("elements"))
                .and_then(|v| v.as_array())
        });

    let elements_arr = match elements_arr {
        Some(arr) => arr,
        None => {
            // No elements found — annotate with empty list (adds "No interactive elements" text)
            let result = annotate_screenshot(&data.screenshot, &[], max_width)
                .map_err(|e| format!("Annotation engine failed: {}", e))?;
            data.screenshot = result.annotated_base64;
            data.element_index_text = Some(result.element_index_text);
            return Ok(());
        }
    };

    // Convert discover elements to AnnotatedElement
    let interactive_types = [
        "button", "input", "select", "textarea", "link", "checkbox", "radio", "a",
    ];

    let mut annotated_elements = Vec::new();
    let mut index = 1_u32;

    for el in elements_arr {
        let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Filter to interactive elements only
        if !interactive_types.contains(&el_type) {
            continue;
        }

        // Skip hidden elements
        if let Some(state_val) = el.get("state") {
            let visible = state_val
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !visible {
                continue;
            }
        }

        // Extract normalized rect — try normalizedRect first, then rect
        let normalized_rect = extract_normalized_rect_from_element(el);
        let Some(normalized_rect) = normalized_rect else {
            continue;
        };

        let label = el
            .get("label")
            .and_then(|v| v.as_str())
            .or_else(|| {
                el.get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| el.get("accessibleName").and_then(|v| v.as_str()))
            .unwrap_or("(unlabeled)")
            .to_string();

        // Truncate long labels
        let label = if label.chars().count() > 40 {
            let truncated: String = label.chars().take(37).collect();
            format!("{}...", truncated)
        } else {
            label
        };

        annotated_elements.push(AnnotatedElement {
            index,
            label,
            element_type: el_type.to_string(),
            normalized_rect,
        });

        index += 1;
        if index > 50 {
            debug!("Annotated screenshot: capped at 50 elements");
            break;
        }
    }

    info!(
        "Annotated screenshot: {} elements to overlay",
        annotated_elements.len()
    );

    // Run the annotation engine
    let result = annotate_screenshot(&data.screenshot, &annotated_elements, max_width)
        .map_err(|e| format!("Annotation engine failed: {}", e))?;

    data.screenshot = result.annotated_base64;
    data.element_index_text = Some(result.element_index_text);

    Ok(())
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
// Stuck Screen Diagnosis Handler
// ============================================================================

/// Internal capture result for diagnosis.
struct DiagnosisCapture {
    image: image::DynamicImage,
    base64: String,
    width: i32,
    height: i32,
    source: String,
}

/// Capture the runner window for diagnosis, falling back to primary monitor.
fn capture_for_diagnosis(app_handle: &tauri::AppHandle) -> Result<DiagnosisCapture, String> {
    use base64::Engine;
    use tauri::Manager;

    // Try runner window first
    if let Some(win) = app_handle.get_webview_window(qontinui_runner_lib::get_main_window_label()) {
        let scale = win.scale_factor().unwrap_or(1.0);
        let pos = win.outer_position().unwrap_or_default();
        let size = win.outer_size().unwrap_or_default();

        if size.width > 0 && size.height > 0 {
            match capture_runner_window(
                pos.x,
                pos.y,
                size.width,
                size.height,
                scale,
                "Qontinui Runner",
            ) {
                Ok(data) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&data.screenshot)
                        .map_err(|e| format!("Base64 decode failed: {}", e))?;
                    let img = image::load_from_memory(&bytes)
                        .map_err(|e| format!("Image decode failed: {}", e))?;
                    return Ok(DiagnosisCapture {
                        image: img,
                        base64: data.screenshot,
                        width: data.width,
                        height: data.height,
                        source: "runner_window".to_string(),
                    });
                }
                Err(e) => {
                    warn!(
                        "Runner window capture failed, falling back to monitor: {}",
                        e
                    );
                }
            }
        }
    }

    // Fallback: primary monitor
    let data = capture_monitor_screenshot(None)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data.screenshot)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Image decode failed: {}", e))?;
    Ok(DiagnosisCapture {
        image: img,
        base64: data.screenshot,
        width: data.width,
        height: data.height,
        source: "primary_monitor".to_string(),
    })
}

/// Native pixel-sampling similarity comparator (no WebView required).
///
/// Samples ~10,000 pixels evenly across the image pair and returns a
/// 0.0–1.0 similarity score. Used exclusively by `diagnose-stuck` which
/// runs when the React frontend hasn't loaded and the browser-side
/// `compareVisualRegression` (canvas-based, accurate) is unavailable.
///
/// This is intentionally a rough approximation — it exists to detect
/// "screen is frozen" vs "screen is changing", not for pixel-accurate
/// regression testing. Do not replace with the IPC-based image-diff
/// endpoint, which requires a functioning WebView.
fn sampled_pixel_similarity_native(img1: &image::DynamicImage, img2: &image::DynamicImage) -> f64 {
    let rgba1 = img1.to_rgba8();
    let rgba2 = img2.to_rgba8();

    if rgba1.dimensions() != rgba2.dimensions() {
        return 0.0;
    }

    let (w, h) = rgba1.dimensions();
    let total = w as u64 * h as u64;
    if total == 0 {
        return 1.0;
    }

    let pixels1 = rgba1.as_raw();
    let pixels2 = rgba2.as_raw();

    // Sample ~10,000 pixels evenly for speed
    let step = (total / 10_000u64).max(1) as usize;
    let mut matching = 0u64;
    let mut sampled = 0u64;

    for i in (0..total as usize).step_by(step) {
        let offset = i * 4;
        if offset + 3 >= pixels1.len() {
            break;
        }

        let diff: u32 = (0..4)
            .map(|c| (pixels1[offset + c] as i32 - pixels2[offset + c] as i32).unsigned_abs())
            .sum();

        // Tolerance for rendering/compression artifacts
        if diff <= 20 {
            matching += 1;
        }
        sampled += 1;
    }

    if sampled == 0 {
        1.0
    } else {
        matching as f64 / sampled as f64
    }
}

/// Try to get DOM-based idle signals from the React frontend.
/// Returns None if React hasn't mounted or doesn't respond within timeout.
async fn try_get_dom_signals(state: &Arc<ApiState>, timeout_ms: u64) -> Option<serde_json::Value> {
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        ui_bridge_request_sync(state, "get_idle_status", serde_json::json!({})),
    )
    .await
    {
        Ok(Ok(data)) => Some(data),
        _ => None,
    }
}

/// Diagnose whether the app is stuck on a loading screen.
///
/// Uses native screenshot capture (xcap) to compare visual state across an
/// observation window. Optionally enriches with DOM signals from the React
/// UI Bridge if it's responsive. Works even if React hasn't mounted.
pub async fn ui_bridge_diagnose_stuck_screen_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Diagnose stuck screen (native)");

    let observation_ms = body
        .get("observationWindowMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(3000);

    // Phase 1: Capture initial screenshot
    let app_handle1 = state.app_handle.clone();
    let cap1 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle1)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: initial screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 2: Try to get DOM signals (short timeout — don't block if React hasn't mounted)
    let dom_status1 = try_get_dom_signals(&state, 2000).await;

    // Phase 3: Wait observation window
    tokio::time::sleep(std::time::Duration::from_millis(observation_ms)).await;

    // Phase 4: Capture second screenshot
    let app_handle2 = state.app_handle.clone();
    let cap2 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle2)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: second screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Second screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 5: Try DOM signals again
    let dom_status2 = try_get_dom_signals(&state, 2000).await;

    // Phase 6: Compare screenshots
    let img1 = cap1.image;
    let img2 = cap2.image;
    let similarity =
        tokio::task::spawn_blocking(move || sampled_pixel_similarity_native(&img1, &img2))
            .await
            .unwrap_or(0.5); // Couldn't compare — inconclusive
    let screenshot_changed = similarity < 0.95;

    // Phase 7: Extract DOM signal details
    let ui_bridge_responsive = dom_status1.is_some() || dom_status2.is_some();

    let dom_ref = dom_status2.as_ref().or(dom_status1.as_ref());
    let signals = dom_ref.and_then(|d| d.get("signals"));

    let has_loading_indicators = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let loading_indicators_list = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("status"))
        .and_then(|s| s.get("indicators"))
        .cloned()
        .unwrap_or(serde_json::json!([]));

    let network_busy = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let pending_requests = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("status"))
        .and_then(|s| s.get("pendingCount"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);

    // Phase 8: Determine verdict
    let obs_secs = observation_ms / 1000;

    let (verdict, confidence, summary, suggestions) =
        if !screenshot_changed && !ui_bridge_responsive {
            (
                "stuck",
                0.95f64,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window and the UI Bridge is not responding \
                     (React may not have mounted)."
                ),
                vec![
                    "Check if the Tauri webview loaded successfully.",
                    "Check the browser console for JavaScript errors.",
                    "Check if the API server started (ports 9876-9878).",
                    "Try restarting the runner.",
                ],
            )
        } else if !screenshot_changed && has_loading_indicators {
            (
                "stuck",
                0.95,
                format!(
                    "The app appears stuck on a loading screen. Loading indicators \
                     are visible, the screen has not changed during the {obs_secs}s \
                     observation window, and no content is being rendered."
                ),
                vec![
                    "Check if a required backend service is running.",
                    "Check the browser console for JavaScript errors.",
                    "Try refreshing the page.",
                ],
            )
        } else if !screenshot_changed && network_busy {
            (
                "stuck",
                0.7,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window but {pending_requests} network \
                     request(s) are still in flight. A request may be hanging."
                ),
                vec![
                    "Check if a network request is hanging.",
                    "Verify the API server is reachable.",
                ],
            )
        } else if !screenshot_changed && ui_bridge_responsive && !has_loading_indicators {
            (
                "idle",
                0.9,
                "The app appears to be in a normal resting state. No loading \
                 indicators detected and the screen is stable."
                    .to_string(),
                vec![],
            )
        } else if screenshot_changed && has_loading_indicators {
            (
                "loading",
                0.85,
                format!(
                    "The app is loading. The screen changed during the {obs_secs}s \
                     observation window and loading indicators are visible, indicating \
                     content is being rendered."
                ),
                vec![],
            )
        } else if screenshot_changed && !ui_bridge_responsive {
            (
                "unknown",
                0.5,
                "The screen is changing but the UI Bridge is not responding. \
                 The app may be loading or recovering."
                    .to_string(),
                vec!["Wait a few seconds and try again."],
            )
        } else {
            (
                "idle",
                0.7,
                "The app appears to be in a normal state. The screen changed \
                 slightly during observation but no loading indicators are present."
                    .to_string(),
                vec![],
            )
        };

    let evidence = serde_json::json!({
        "screenshotSimilarity": similarity,
        "screenshotChanged": screenshot_changed,
        "uiBridgeResponsive": ui_bridge_responsive,
        "loadingIndicators": loading_indicators_list,
        "networkBusy": network_busy,
        "pendingNetworkRequests": pending_requests,
    });

    let diagnosis = serde_json::json!({
        "verdict": verdict,
        "confidence": confidence,
        "summary": summary,
        "evidence": evidence,
        "observationWindowMs": observation_ms,
        "suggestions": suggestions,
        "screenshot": cap2.base64,
        "screenshotWidth": cap2.width,
        "screenshotHeight": cap2.height,
        "captureSource": cap2.source,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    });

    Json(ApiResponse::success(diagnosis))
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
// Element image capture + element screenshot
// ============================================================================

/// `GET /ui-bridge/control/element-screenshot?id=...`
///
/// Returns a single element's PNG capture. Wraps the existing
/// `capture_element_images` batch IPC type — we ask the webview to capture
/// just one element and unwrap the single-entry result.
pub async fn ui_bridge_element_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let id = match query.get("id") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "element-screenshot requires `id` query param".to_string(),
                )),
            ));
        }
    };

    // Use the dedicated capture_single_element IPC type which has
    // multiple DOM lookup fallbacks (registry → getElementById →
    // data-ui-bridge-id → title → aria-label). The old batch path
    // (capture_element_images) only looked in the bridge registry
    // and returned 0 captures for elements that weren't persistently
    // registered.
    let payload = serde_json::json!({
        "params": { "elementId": id.clone() }
    });

    match ui_bridge_request_sync(&state, "capture_single_element", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            if e.to_lowercase().contains("not found") {
                Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!(
                        "element '{}' not found or not capturable",
                        id
                    ))),
                ))
            } else {
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
            }
        }
    }
}

/// POST /ui-bridge/control/capture-element-images — Capture element images directly from the DOM.
///
/// Uses html2canvas in the frontend to render each element to a canvas, bypassing
/// screen capture entirely. This produces correct images even when other windows
/// cover the runner.
pub async fn ui_bridge_capture_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Capture element images");

    // Use a longer timeout for element capture — html2canvas rendering 30+ elements
    // can take 30-60 seconds. The default 10s UI Bridge IPC timeout is too short.
    let request_id = uuid::Uuid::new_v4().to_string();
    let event_payload = serde_json::json!({
        "requestId": request_id,
        "type": "capture_element_images",
        "params": body,
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&request_id).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to emit request: {}", e))),
        ));
    }

    // 120 second timeout for element capture (vs 10s default)
    let timeout = std::time::Duration::from_secs(120);
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(data)) => Ok(Json(ApiResponse::success(data))),
        Ok(Err(_)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Request channel closed".to_string())),
        )),
        Err(_) => {
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            error!("UI Bridge API: Capture element images timed out after 120s");
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error(
                    "Element capture timed out after 120s".to_string(),
                )),
            ))
        }
    }
}

/// POST /ui-bridge/control/get-element-images — Read <img> src attributes from the DOM.
pub async fn ui_bridge_get_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get element images");

    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "get_element_images", payload).await)
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

// ============================================================================
// Media routes (IPC to webview SDK)
// ============================================================================

/// Find media elements.
pub async fn ui_bridge_media_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "find_media", payload).await)
}

/// Media audit (accessibility or performance).
pub async fn ui_bridge_media_audit_handler(
    State(state): State<Arc<ApiState>>,
    Path(audit_type): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": { "auditType": audit_type } });
    wrap_ipc_result(ui_bridge_request_sync(&state, "media_audit", payload).await)
}

/// Capture media snapshot.
pub async fn ui_bridge_media_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "capture_media_snapshot", payload).await)
}

/// Analyze media elements.
pub async fn ui_bridge_media_analyze_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "analyze_media", payload).await)
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::{get, post};
    let router = axum::Router::new()
        .route(
            "/ui-bridge/control/annotated-screenshot",
            get(ui_bridge_annotated_screenshot_handler),
        )
        .route(
            "/ui-bridge/control/diagnose-stuck",
            post(ui_bridge_diagnose_stuck_screen_handler),
        )
        .route(
            "/ui-bridge/control/page-health",
            post(ui_bridge_page_health_handler),
        );
    // element-screenshot: identical handler under /control + /ai.
    let router = add_dual!(
        router,
        get,
        "element-screenshot",
        ui_bridge_element_screenshot_handler
    );
    router
        .route(
            "/ui-bridge/control/capture-element-images",
            post(ui_bridge_capture_element_images_handler),
        )
        .route(
            "/ui-bridge/control/get-element-images",
            post(ui_bridge_get_element_images_handler),
        )
        // Annotations CRUD
        .route(
            "/ui-bridge/control/annotations",
            get(ui_bridge_annotations_list_handler).post(ui_bridge_annotations_create_handler),
        )
        .route(
            "/ui-bridge/control/annotation/{id}",
            get(ui_bridge_annotations_get_handler)
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
        // Media routes
        .route(
            "/ui-bridge/ai/media/find",
            post(ui_bridge_media_find_handler),
        )
        .route(
            "/ui-bridge/ai/media/audit/{audit_type}",
            post(ui_bridge_media_audit_handler),
        )
        .route(
            "/ui-bridge/ai/media/snapshot",
            post(ui_bridge_media_snapshot_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/batch",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/page",
            post(ui_bridge_media_analyze_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/annotated-screenshot"),
        ("POST", "/ui-bridge/control/diagnose-stuck"),
        ("POST", "/ui-bridge/control/page-health"),
        ("GET", "/ui-bridge/control/element-screenshot"),
        ("GET", "/ui-bridge/ai/element-screenshot"),
        ("POST", "/ui-bridge/control/capture-element-images"),
        ("POST", "/ui-bridge/control/get-element-images"),
        ("GET", "/ui-bridge/control/annotations"),
        ("POST", "/ui-bridge/control/annotations"),
        ("GET", "/ui-bridge/control/annotation/{id}"),
        ("PUT", "/ui-bridge/control/annotation/{id}"),
        ("DELETE", "/ui-bridge/control/annotation/{id}"),
        ("GET", "/ui-bridge/control/annotations/coverage"),
        ("GET", "/ui-bridge/control/annotations/export"),
        ("POST", "/ui-bridge/ai/media/find"),
        ("POST", "/ui-bridge/ai/media/audit/{audit_type}"),
        ("POST", "/ui-bridge/ai/media/snapshot"),
        ("POST", "/ui-bridge/ai/media/analyze"),
        ("POST", "/ui-bridge/ai/media/analyze/batch"),
        ("POST", "/ui-bridge/ai/media/analyze/page"),
    ]
}
