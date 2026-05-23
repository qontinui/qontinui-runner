//! Frame-source decoupling for the `/ui-bridge/vision/*` pipeline.
//!
//! Historically every vision handler captured the runner's *own* desktop
//! window via [`capture_runner_window_frame`]. That made the vision skills
//! unusable against a remote target (a paired phone, an HTTP-registered app):
//! pointing them at a device id silently analyzed the runner desktop instead.
//!
//! This module introduces a [`FrameProvider`] abstraction so a handler can
//! source its [`Frame`] from a *target* device/app instead. The resolution is
//! driven by an optional `target` field on each request:
//!
//!   - `None`            → [`RunnerWindowSource`] — byte-identical to the
//!     pre-existing behavior (delegates straight to
//!     [`capture_runner_window_frame`]).
//!   - registered phone  → [`DeviceScreenshotSource`] hitting the device's
//!     proxy `…/ui-bridge/control/screenshot` endpoint.
//!   - registered app    → [`DeviceScreenshotSource`] hitting the app's own
//!     base url.
//!   - adb serial         → [`AdbScreencapSource`] pulling the framebuffer.
//!
//! The runner ships no vision models; these sources only *acquire pixels*. The
//! downstream pipeline (crop / encode / OCR / VLM) is untouched.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use qontinui_vision_core::{Frame, FrameSource, FrameSourceKind};

use crate::mcp::adb_helper;
use crate::mcp::types::ApiState;

use super::vision_routes::capture_runner_window_frame;

/// Timeout for the remote `control/screenshot` fetch.
const SCREENSHOT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// A source of a single [`Frame`] for the vision pipeline.
///
/// Implementors acquire pixels from somewhere (the runner desktop, a remote
/// device's HTTP endpoint, an adb framebuffer) and wrap them in a `Frame` with
/// an appropriate [`FrameSource`]. Errors are returned as `String` to match
/// the existing capture-fn error contract in `vision_routes`.
#[async_trait]
pub trait FrameProvider: Send + Sync {
    async fn frame(&self, state: &Arc<ApiState>) -> Result<Frame, String>;
}

/// Captures the runner's own desktop window. This is the legacy behavior and
/// the resolution target for `target == None`; it delegates directly to the
/// untouched [`capture_runner_window_frame`] so the no-target path is
/// byte-identical to pre-decoupling.
pub struct RunnerWindowSource;

#[async_trait]
impl FrameProvider for RunnerWindowSource {
    async fn frame(&self, state: &Arc<ApiState>) -> Result<Frame, String> {
        capture_runner_window_frame(state).await
    }
}

/// Fetches a screenshot over HTTP from a target that speaks the UI Bridge
/// `control/screenshot` contract (a paired phone via its proxy url, or an
/// HTTP-registered app via its base url).
///
/// Response shape (tolerated both enveloped and flat):
/// ```json
/// { "success": true,
///   "data": { "screenshot": "<base64 png>", "width": <logical>, "height": <logical> },
///   "timestamp": ... }
/// ```
/// `width` is in *logical points*; the decoded PNG is *physical px*, so the
/// HiDPI scale factor is `decoded.width() / reported_width`.
pub struct DeviceScreenshotSource {
    /// Base url, e.g. `http://127.0.0.1:8087`. Trailing slash tolerated.
    pub base_url: String,
}

#[async_trait]
impl FrameProvider for DeviceScreenshotSource {
    async fn frame(&self, _state: &Arc<ApiState>) -> Result<Frame, String> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/ui-bridge/control/screenshot");

        let client = reqwest::Client::builder()
            .timeout(SCREENSHOT_HTTP_TIMEOUT)
            .build()
            .map_err(|e| format!("build screenshot client: {e}"))?;

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("device screenshot request to {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "device screenshot {url} returned HTTP {}",
                resp.status()
            ));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("device screenshot {url} bad JSON: {e}"))?;

        frame_from_screenshot_json(&body).map_err(|e| format!("device at {base}: {e}"))
    }
}

/// Decode a `control/screenshot` JSON body into a [`Frame`]. Pure (no I/O) so
/// the parsing + HiDPI-scale logic is unit-testable without a live device.
///
/// Tolerates both the enveloped shape (`{ data: { screenshot, width } }`) and a
/// flat top-level object. `width` is *logical points*; the decoded PNG is
/// *physical px*, so `scale_factor = decoded.width() / reported_width` (default
/// 1.0 when width is absent/zero).
fn frame_from_screenshot_json(body: &serde_json::Value) -> Result<Frame, String> {
    // Prefer the `data` envelope; fall back to a flat top-level object.
    let payload = body.get("data").unwrap_or(body);

    let b64 = payload
        .get("screenshot")
        .and_then(|v| v.as_str())
        .ok_or("returned no `screenshot` field — target has no screenshotProvider configured")?;

    let png_bytes = BASE64
        .decode(b64)
        .map_err(|e| format!("screenshot base64 decode: {e}"))?;
    let rgba = image::load_from_memory(&png_bytes)
        .map_err(|e| format!("screenshot image decode: {e}"))?
        .to_rgba8();

    let reported_width = payload.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let scale_factor = if reported_width > 0.0 {
        f64::from(rgba.width()) / reported_width
    } else {
        1.0
    };

    Ok(Frame::from_rgba(
        rgba,
        FrameSource {
            kind: FrameSourceKind::Device,
            scale_factor,
            captured_at: Utc::now(),
        },
    ))
}

/// Pulls a device framebuffer directly over adb (`screenshot_png`). The
/// framebuffer is already physical pixels, so `scale_factor` is `1.0`.
pub struct AdbScreencapSource {
    pub serial: String,
}

#[async_trait]
impl FrameProvider for AdbScreencapSource {
    async fn frame(&self, _state: &Arc<ApiState>) -> Result<Frame, String> {
        let png_bytes = adb_helper::screenshot_png(self.serial.clone()).await?;
        let rgba = image::load_from_memory(&png_bytes)
            .map_err(|e| format!("adb framebuffer image decode: {e}"))?
            .to_rgba8();
        Ok(Frame::from_rgba(
            rgba,
            FrameSource {
                kind: FrameSourceKind::Device,
                scale_factor: 1.0,
                captured_at: Utc::now(),
            },
        ))
    }
}

/// Resolve a [`FrameProvider`] for an optional vision `target`.
///
/// `None` reproduces today's behavior exactly ([`RunnerWindowSource`]). When a
/// target id is supplied it is matched, in priority order, against:
///   1. a registered physical device (its active proxy url),
///   2. a registered app (its base url),
///   3. an adb serial / emulator transport id,
/// and otherwise rejected with a clear error.
pub async fn resolve_frame_provider(
    state: &Arc<ApiState>,
    target: &Option<String>,
) -> Result<Box<dyn FrameProvider>, String> {
    let id = match target {
        None => return Ok(Box::new(RunnerWindowSource)),
        Some(id) => id,
    };

    // 1. Registered physical device → proxy screenshot.
    if let Some(base_url) = state
        .physical_device_registry
        .get_active_proxy_url(id)
        .await
    {
        return Ok(Box::new(DeviceScreenshotSource { base_url }));
    }

    // 2. Registered app → its base url.
    if let Some(entry) = state.app_registry.get(id).await {
        return Ok(Box::new(DeviceScreenshotSource {
            base_url: entry.app.url.clone(),
        }));
    }

    // 3. adb serial / emulator transport id → framebuffer.
    if id.starts_with("emulator-") || adb_helper::is_adb_serial(id) {
        return Ok(Box::new(AdbScreencapSource { serial: id.clone() }));
    }

    Err(format!("unknown vision target '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use image::{ImageFormat, RgbaImage};
    use std::io::Cursor;

    /// Encode a solid `w×h` physical-px RGBA PNG as a base64 string.
    fn png_b64(w: u32, h: u32) -> String {
        let img = RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, ImageFormat::Png)
            .expect("encode png");
        BASE64.encode(buf.into_inner())
    }

    #[test]
    fn enveloped_response_computes_hidpi_scale() {
        // 200×100 physical px, reported logical width 100 ⇒ scale 2.0 (Retina).
        let body = serde_json::json!({
            "success": true,
            "data": { "screenshot": png_b64(200, 100), "width": 100, "height": 50 },
            "timestamp": 0
        });
        let frame = frame_from_screenshot_json(&body).expect("decode");
        assert_eq!((frame.width, frame.height), (200, 100));
        assert_eq!(frame.source.kind, FrameSourceKind::Device);
        assert!((frame.source.scale_factor - 2.0).abs() < 1e-9);
    }

    #[test]
    fn flat_response_without_envelope_is_tolerated() {
        // No `data` wrapper; width matches physical ⇒ scale 1.0.
        let body = serde_json::json!({ "screenshot": png_b64(120, 80), "width": 120 });
        let frame = frame_from_screenshot_json(&body).expect("decode");
        assert_eq!((frame.width, frame.height), (120, 80));
        assert!((frame.source.scale_factor - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_width_defaults_scale_to_one() {
        let body = serde_json::json!({ "data": { "screenshot": png_b64(64, 64) } });
        let frame = frame_from_screenshot_json(&body).expect("decode");
        assert!((frame.source.scale_factor - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_screenshot_field_fails_loudly() {
        // The whole point of the decoupling: a target with no screenshotProvider
        // must error, never silently fall back to a runner-window capture.
        let body = serde_json::json!({ "success": true, "data": { "width": 100 } });
        let err = frame_from_screenshot_json(&body).expect_err("must error");
        assert!(
            err.contains("screenshotProvider"),
            "error should name the missing provider, got: {err}"
        );
    }

    #[test]
    fn malformed_base64_is_an_error_not_a_panic() {
        let body = serde_json::json!({ "data": { "screenshot": "not-base64!!!", "width": 10 } });
        assert!(frame_from_screenshot_json(&body).is_err());
    }
}
