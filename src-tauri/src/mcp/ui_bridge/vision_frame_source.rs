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
            // A short connect timeout so a dead/unreachable host fails fast
            // instead of stalling the whole vision call on the (longer) total
            // read timeout.
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| format!("build screenshot client: {e}"))?;

        let resp = client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                format!(
                    "device screenshot at {url} did not respond within {SCREENSHOT_HTTP_TIMEOUT:?} \
                     — the device app may be backgrounded or its screen off"
                )
            } else {
                format!("device screenshot request to {url}: {e}")
            }
        })?;
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

    // Before treating a missing `screenshot` field as "no provider", check for
    // an explicit failure envelope. A backgrounded phone / screen-off device
    // answers the request but reports `success: false` and/or a non-empty
    // `error`; relaying the device's own reason is far more useful than the
    // misleading "no screenshotProvider configured" fallback below. Look at
    // both the `data` envelope and the top level (the failure flag often lives
    // at the top while a `data` object may be absent or empty).
    let explicit_failure = |v: &serde_json::Value| -> Option<String> {
        let success_false = v.get("success").and_then(|s| s.as_bool()) == Some(false);
        let error_text = v
            .get("error")
            .and_then(|e| e.as_str())
            .filter(|s| !s.trim().is_empty());
        if success_false || error_text.is_some() {
            Some(error_text.unwrap_or("unknown error").to_string())
        } else {
            None
        }
    };
    if let Some(error) = explicit_failure(body).or_else(|| explicit_failure(payload)) {
        return Err(format!("device screenshot capture failed: {error}"));
    }

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
            capture_backend: None,
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
                capture_backend: None,
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
    //
    // The registries are now exhausted. `is_adb_serial` is a permissive
    // syntactic classifier — an arbitrary label like `bogus-id` is alphanumeric
    // with a dash and so *looks* like a USB serial. Routing every such string to
    // adb produced a misleading "ADB device … not found" error for what is
    // really an unknown target. Gate on actual adb presence: a string only goes
    // to `AdbScreencapSource` when it is serial-shaped AND adb currently lists it
    // as a connected device. A real serial still routes; `bogus-id` falls through
    // to the intended "unknown vision target" error.
    let connected = adb_helper::is_connected_device(id).await;
    if should_route_to_adb(id, connected) {
        return Ok(Box::new(AdbScreencapSource { serial: id.clone() }));
    }

    Err(format!("unknown vision target '{id}'"))
}

/// Pure routing decision for step 3 of [`resolve_frame_provider`]: a target the
/// device + app registries did not claim is routed to adb only when it is
/// serial-shaped AND adb reports it as a connected device.
///
/// Separated from the async resolver (which needs an `ApiState` + a live adb
/// server) so the bogus-id-vs-real-serial distinction is unit-testable in
/// isolation. `connected` is the result of [`adb_helper::is_connected_device`].
fn should_route_to_adb(id: &str, connected: bool) -> bool {
    let serial_shaped = id.starts_with("emulator-") || adb_helper::is_adb_serial(id);
    serial_shaped && connected
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
    fn explicit_failure_envelope_relays_device_error() {
        // A backgrounded / screen-off device answers the request but reports an
        // explicit failure. We must surface the device's own reason, NOT the
        // misleading "no screenshotProvider configured" fallback.
        let body = serde_json::json!({ "success": false, "error": "app not foregrounded" });
        let err = frame_from_screenshot_json(&body).expect_err("must error");
        assert!(
            err.contains("capture failed"),
            "error should say capture failed, got: {err}"
        );
        assert!(
            err.contains("app not foregrounded"),
            "error should relay the device's reason, got: {err}"
        );
        // It must NOT misreport as a missing-provider condition.
        assert!(
            !err.contains("screenshotProvider"),
            "an explicit device failure must not be reported as a missing provider, got: {err}"
        );
    }

    #[test]
    fn success_shaped_without_screenshot_reports_missing_provider() {
        // A response that looks successful (no failure flag, no error) but
        // simply lacks a `screenshot` field is the genuine no-provider case.
        let body = serde_json::json!({ "success": true, "data": { "width": 100 } });
        let err = frame_from_screenshot_json(&body).expect_err("must error");
        assert!(
            err.contains("screenshotProvider"),
            "error should name the missing provider, got: {err}"
        );
        assert!(
            !err.contains("capture failed"),
            "a missing field is not an explicit capture failure, got: {err}"
        );
    }

    #[test]
    fn malformed_base64_is_an_error_not_a_panic() {
        let body = serde_json::json!({ "data": { "screenshot": "not-base64!!!", "width": 10 } });
        assert!(frame_from_screenshot_json(&body).is_err());
    }

    // -----------------------------------------------------------------------
    // adb routing gate (step 3 of resolve_frame_provider)
    // -----------------------------------------------------------------------

    #[test]
    fn bogus_id_does_not_route_to_adb() {
        // `bogus-id` is syntactically serial-shaped (alphanumeric + dash), so the
        // permissive `is_adb_serial` classifier accepts it. The presence gate is
        // what saves us: adb does not list it, so it must NOT route to adb and
        // the resolver falls through to "unknown vision target '<id>'".
        assert!(
            adb_helper::is_adb_serial("bogus-id"),
            "precondition: bogus-id is serial-shaped, so syntax alone can't reject it"
        );
        assert!(
            !should_route_to_adb("bogus-id", false),
            "an unknown, not-connected id must not route to adb"
        );
    }

    #[test]
    fn real_serial_routes_to_adb_when_connected() {
        // A genuine USB serial that adb lists routes to AdbScreencapSource.
        assert!(should_route_to_adb("R3CN30ABCDE", true));
        // An emulator transport id that adb lists also routes.
        assert!(should_route_to_adb("emulator-5554", true));
    }

    #[test]
    fn serial_shaped_but_absent_does_not_route() {
        // Even a perfectly serial-shaped id is rejected if adb doesn't list it —
        // e.g. a device that was unplugged. Better to report "unknown target"
        // than to hand a dead serial to the framebuffer puller.
        assert!(!should_route_to_adb("R3CN30ABCDE", false));
        assert!(!should_route_to_adb("emulator-5554", false));
    }

    #[test]
    fn non_serial_shaped_never_routes_even_if_claimed_connected() {
        // Defense in depth: a string that isn't serial-shaped never routes,
        // regardless of the presence flag.
        assert!(!should_route_to_adb("has spaces", true));
        assert!(!should_route_to_adb("app/with/slashes", true));
    }
}
