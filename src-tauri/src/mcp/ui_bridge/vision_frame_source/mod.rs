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
//!   - `None`            → [`RunnerWindowSource`] — the capture itself is
//!     byte-identical to the pre-existing behavior (delegates straight to
//!     [`capture_runner_window_frame`]). One visible difference: a
//!     soft-failing route's `frame_error` now carries a `CODE: ` prefix
//!     ([`FrameSourceError::to_frame_error`]).
//!   - registered phone  → [`DeviceScreenshotSource`] hitting the device's
//!     proxy `…/ui-bridge/control/screenshot` endpoint.
//!   - registered app    → by the app's TRANSPORT ([`provider_for_app`]):
//!     an HTTP app gets [`DeviceScreenshotSource`] against its own base url;
//!     a WebSocket app that advertises the [`SCREENSHOT_CAPABILITY`] gets
//!     [`WsRelayScreenshotSource`], which relays [`SCREENSHOT_ACTION`] over
//!     its `/ui-bridge/ws` socket; one that does not is REFUSED with
//!     [`FrameSourceError::Unsupported`] (plan
//!     `2026-11-21-vision-capture-cannot-frame-a-websocket-transport-app`).
//!   - adb serial         → [`AdbScreencapSource`] pulling the framebuffer.
//!
//! Every failure is a typed [`FrameSourceError`] carrying its own HTTP status
//! and machine-readable `code`, so a refusal (`400`/`422`) never reads as a
//! server fault (`500`) and an app that could not be reached (`502`) never
//! reads as the runner's own.
//!
//! The runner ships no vision models; these sources only *acquire pixels*. The
//! downstream pipeline (crop / encode / OCR / VLM) is untouched.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::StatusCode;
use axum::Json;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use qontinui_vision_core::{Frame, FrameSource, FrameSourceKind};

use crate::mcp::adb_helper;
use crate::mcp::app_registry::{AppTransport, RegisteredApp};
use crate::mcp::types::{ApiResponse, ApiState};

use super::vision_routes::capture_runner_window_frame;

pub mod ws_relay_source;

pub use ws_relay_source::{WsRelayScreenshotSource, SCREENSHOT_ACTION, SCREENSHOT_CAPABILITY};

/// Timeout for the remote `control/screenshot` fetch.
const SCREENSHOT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Every frame source this runner BINARY can acquire pixels from — one entry
/// per [`FrameProvider::source_label`]. Served on `/health` as
/// `visionFrameSources` so a client (ui-bridge-mcp) can tell a build that
/// captures a WebSocket-transport app over its socket (`websocket_relay`)
/// from one that predates the arm (field absent). A capability of the runner
/// itself, deliberately NOT an entry in `sdkFeatures` (bundled SDK features).
pub const VISION_FRAME_SOURCES: &[&str] =
    &["runner_window", "device_http", "adb", "websocket_relay"];

/// Why a vision target's frame could not be acquired. Each variant carries
/// its HTTP [`status`](Self::status) and machine-readable
/// [`code`](Self::code); the call sites in `vision_routes` map it through
/// [`Self::to_json_err`] (or, where a capture failure is soft, carry
/// [`Self::to_frame_error`]) instead of flattening every failure to `500`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameSourceError {
    /// The id is not a registered device, a live registered app, or a
    /// connected adb serial. `400 UNKNOWN_VISION_TARGET`.
    #[error("unknown vision target '{0}'")]
    UnknownTarget(String),

    /// The target exists but cannot supply a frame over its transport — a
    /// REFUSAL by name, never a fake frame. `422 FRAME_SOURCE_UNSUPPORTED`.
    #[error("vision target '{target}' ({transport} transport) cannot be captured: {reason}")]
    Unsupported {
        target: String,
        transport: &'static str,
        reason: String,
    },

    /// The app could not be reached, or did not answer in time, or answered
    /// unreadably — the outcome is the TRANSPORT's, not the app's. `502` with
    /// `code` `APP_UNREACHABLE` or `APP_RESPONSE_UNREADABLE`. `retryable` is
    /// `None` when unknown (a timeout, an unreadable answer).
    #[error("{message}")]
    Unreachable {
        message: String,
        code: &'static str,
        retryable: Option<bool>,
    },

    /// The target ANSWERED and reported (or produced) a failed capture.
    /// `502`; `code` is the app's own code when it sent one (`TAB_NOT_VISIBLE`,
    /// `SCREENSHOT_TIMEOUT`, …), else `APP_CAPTURE_FAILED`. `retryable` is the
    /// app's own verdict, `None` when it did not say (UNKNOWN, never `false`).
    #[error("{message}")]
    CaptureFailed {
        message: String,
        code: Option<String>,
        retryable: Option<bool>,
    },

    /// The runner's OWN capture failed (its desktop window, or building its
    /// own HTTP client) — a server fault. `500 RUNNER_CAPTURE_FAILED`. The
    /// message is the capture function's text, unchanged.
    #[error("{0}")]
    RunnerCapture(String),
}

pub const CODE_UNKNOWN_VISION_TARGET: &str = "UNKNOWN_VISION_TARGET";
pub const CODE_FRAME_SOURCE_UNSUPPORTED: &str = "FRAME_SOURCE_UNSUPPORTED";
pub const CODE_APP_UNREACHABLE: &str = "APP_UNREACHABLE";
pub const CODE_APP_RESPONSE_UNREADABLE: &str = "APP_RESPONSE_UNREADABLE";
pub const CODE_APP_CAPTURE_FAILED: &str = "APP_CAPTURE_FAILED";
pub const CODE_RUNNER_CAPTURE_FAILED: &str = "RUNNER_CAPTURE_FAILED";

impl FrameSourceError {
    /// The HTTP status a route answers this failure with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::UnknownTarget(_) => StatusCode::BAD_REQUEST,
            Self::Unsupported { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unreachable { .. } | Self::CaptureFailed { .. } => StatusCode::BAD_GATEWAY,
            Self::RunnerCapture(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The machine-readable SCREAMING_SNAKE_CASE code for the `code` field.
    pub fn code(&self) -> &str {
        match self {
            Self::UnknownTarget(_) => CODE_UNKNOWN_VISION_TARGET,
            Self::Unsupported { .. } => CODE_FRAME_SOURCE_UNSUPPORTED,
            Self::Unreachable { code, .. } => code,
            Self::CaptureFailed { code, .. } => code.as_deref().unwrap_or(CODE_APP_CAPTURE_FAILED),
            Self::RunnerCapture(_) => CODE_RUNNER_CAPTURE_FAILED,
        }
    }

    /// `hint` for the two failures that came from beyond the runner, named
    /// the way `/ui-bridge/apps/{id}/dispatch` names them
    /// (`app_discovery::app_failure_response`): `failure_origin` is `"app"`
    /// when the target itself reported the failure, `"transport"` otherwise,
    /// and `retryable` is `null` when unknown.
    fn hint(&self) -> Option<serde_json::Value> {
        let (origin, retryable) = match self {
            Self::Unreachable { retryable, .. } => ("transport", *retryable),
            Self::CaptureFailed { retryable, .. } => ("app", *retryable),
            _ => return None,
        };
        Some(serde_json::json!({ "failure_origin": origin, "retryable": retryable }))
    }

    /// The `(status, {success:false, error, code, hint?})` error a `Json`
    /// vision route returns.
    pub fn to_json_err(&self) -> (StatusCode, Json<ApiResponse<()>>) {
        let mut body = ApiResponse::error_with_code(self.to_string(), self.code());
        body.hint = self.hint();
        (self.status(), Json(body))
    }

    /// The text a SOFT-failing route (one that continues snapshot-only)
    /// carries in its `frame_error` field: `CODE: message`, the shape a
    /// wrapper failure already renders as, so the code survives a string.
    pub fn to_frame_error(&self) -> String {
        format!("{}: {}", self.code(), self)
    }
}

/// A source of a single [`Frame`] for the vision pipeline.
///
/// Implementors acquire pixels from somewhere (the runner desktop, a remote
/// device's HTTP endpoint, an adb framebuffer, a WebSocket client's own
/// capture handle) and wrap them in a `Frame` with an appropriate
/// [`FrameSource`].
#[async_trait]
pub trait FrameProvider: Send + Sync {
    async fn frame(&self, state: &Arc<ApiState>) -> Result<Frame, FrameSourceError>;

    /// Which [`VISION_FRAME_SOURCES`] entry this provider is.
    fn source_label(&self) -> &'static str;
}

/// Captures the runner's own desktop window. This is the legacy behavior and
/// the resolution target for `target == None`; it delegates directly to the
/// untouched [`capture_runner_window_frame`], so the no-target capture is
/// byte-identical to pre-decoupling (only a soft-failing route's
/// `frame_error` text gains its `CODE: ` prefix).
pub struct RunnerWindowSource;

#[async_trait]
impl FrameProvider for RunnerWindowSource {
    async fn frame(&self, state: &Arc<ApiState>) -> Result<Frame, FrameSourceError> {
        capture_runner_window_frame(state)
            .await
            .map_err(FrameSourceError::RunnerCapture)
    }

    fn source_label(&self) -> &'static str {
        "runner_window"
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
    /// The target is a registered phone: its health is probed first, and the
    /// request is signed when the phone requires pairing
    /// ([`crate::mcp::sdk_http::probe_device_client`]).
    pub phone: bool,
    /// The vision target id the caller named, so a refusal can say which
    /// target it refused.
    pub target: String,
}

impl DeviceScreenshotSource {
    /// The source for an HTTP-registered app.
    pub fn for_app(app_id: &str, entry: &RegisteredApp) -> Self {
        Self {
            base_url: entry.app.url.clone(),
            phone: false,
            target: app_id.to_string(),
        }
    }

    /// The HTTP fetch itself — it needs no `ApiState`.
    pub async fn fetch(&self) -> Result<Frame, FrameSourceError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/ui-bridge/control/screenshot");
        let unreachable =
            |message: String, retryable: Option<bool>| FrameSourceError::Unreachable {
                message,
                code: CODE_APP_UNREACHABLE,
                retryable,
            };

        let client = reqwest::Client::builder()
            .timeout(SCREENSHOT_HTTP_TIMEOUT)
            // A short connect timeout so a dead/unreachable host fails fast
            // instead of stalling the whole vision call on the (longer) total
            // read timeout.
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| {
                FrameSourceError::RunnerCapture(format!("build screenshot client: {e}"))
            })?;
        let client = if self.phone {
            crate::mcp::sdk_http::probe_device_client(base, client)
                .await
                .map_err(|e| unreachable(format!("device screenshot at {url}: {e}"), None))?
        } else {
            crate::mcp::sdk_http::SdkHttpClient::unsigned(client)
        };

        let resp = client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                unreachable(
                    format!(
                        "device screenshot at {url} did not respond within {SCREENSHOT_HTTP_TIMEOUT:?} \
                         — the device app may be backgrounded or its screen off"
                    ),
                    None,
                )
            } else {
                unreachable(format!("device screenshot request to {url}: {e}"), Some(true))
            }
        })?;
        if !resp.status().is_success() {
            let status = resp.status();
            // An HTTP app that 404s the route has no screenshot door at all:
            // name the cause instead of the bare status (plan
            // 2026-11-21-vision-capture-cannot-frame-a-websocket-transport-app,
            // Phase 1 finish-to-zero). A phone's proxy keeps its own reason.
            if status == reqwest::StatusCode::NOT_FOUND && !self.phone {
                return Err(FrameSourceError::Unsupported {
                    target: self.target.clone(),
                    transport: "http",
                    reason: format!(
                        "app '{}' at {base} does not serve /ui-bridge/control/screenshot \
                         — it has no screenshotProvider, or its registered URL is a page origin",
                        self.target
                    ),
                });
            }
            // Read the body FIRST: a UI Bridge envelope carrying a `code` means
            // the device itself answered — e.g. ui-bridge-native's own 503
            // `PAIRING_UNAVAILABLE` refusal — so it is the app's failure,
            // with its code, whatever the status.
            let body = resp.json::<serde_json::Value>().await.ok();
            // A pairing refusal's `error` says what to do; keep it.
            let reason = body
                .as_ref()
                .and_then(|b| b.get("error")?.as_str().map(str::to_string));
            let app_code = body.as_ref().and_then(envelope_code);
            let message = match reason {
                Some(reason) => format!("device screenshot {url} returned HTTP {status}: {reason}"),
                None => format!("device screenshot {url} returned HTTP {status}"),
            };
            // With no such envelope, a gateway status (bad gateway,
            // unavailable, gateway timeout) is most likely a proxy answering
            // for a device it could not reach: the transport's failure.
            let gateway = matches!(
                status,
                reqwest::StatusCode::BAD_GATEWAY
                    | reqwest::StatusCode::SERVICE_UNAVAILABLE
                    | reqwest::StatusCode::GATEWAY_TIMEOUT
            );
            return Err(match app_code {
                Some(code) => FrameSourceError::CaptureFailed {
                    message,
                    code: Some(code),
                    retryable: body.as_ref().and_then(|b| b.get("retryable")?.as_bool()),
                },
                None if gateway => unreachable(message, None),
                None => FrameSourceError::CaptureFailed {
                    message,
                    code: None,
                    retryable: None,
                },
            });
        }
        let body: serde_json::Value =
            resp.json()
                .await
                .map_err(|e| FrameSourceError::Unreachable {
                    message: format!("device screenshot {url} bad JSON: {e}"),
                    code: CODE_APP_RESPONSE_UNREADABLE,
                    retryable: None,
                })?;

        frame_from_screenshot_json(&body).map_err(|e| FrameSourceError::CaptureFailed {
            message: format!("device at {base}: {e}"),
            code: None,
            retryable: None,
        })
    }
}

#[async_trait]
impl FrameProvider for DeviceScreenshotSource {
    async fn frame(&self, _state: &Arc<ApiState>) -> Result<Frame, FrameSourceError> {
        self.fetch().await
    }

    fn source_label(&self) -> &'static str {
        "device_http"
    }
}

/// The machine-readable code of a UI Bridge `{success:false, error, code}`
/// envelope (`code`, else `errorCode`; at the top level or inside an object
/// `error`), when the body is one. `None` for any other body.
fn envelope_code(body: &serde_json::Value) -> Option<String> {
    let text = |v: &serde_json::Value| {
        ["code", "errorCode"]
            .into_iter()
            .find_map(|k| v.get(k)?.as_str())
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
    };
    text(body).or_else(|| body.get("error").filter(|e| e.is_object()).and_then(text))
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
    async fn frame(&self, _state: &Arc<ApiState>) -> Result<Frame, FrameSourceError> {
        let capture_failed = |message: String| FrameSourceError::CaptureFailed {
            message,
            code: None,
            retryable: None,
        };
        let png_bytes = adb_helper::screenshot_png(self.serial.clone())
            .await
            .map_err(capture_failed)?;
        let rgba = image::load_from_memory(&png_bytes)
            .map_err(|e| capture_failed(format!("adb framebuffer image decode: {e}")))?
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

    fn source_label(&self) -> &'static str {
        "adb"
    }
}

/// The reason a WebSocket-transport app with no `screenshot` capability is
/// refused (D2 of the plan).
const WEBSOCKET_NO_CAPABILITY_REASON: &str =
    "the app is connected over /ui-bridge/ws and does not \
     advertise the `screenshot` capability (a plain in-page SDK tab cannot capture itself; a \
     wrapper or the extension live relay can)";

/// Refuse a WebSocket-transport app that does not advertise
/// [`SCREENSHOT_CAPABILITY`]: sending it `getScreenshot` blind would answer
/// `NO_HANDLER`, which reads as an app fault rather than the refusal it is.
pub(crate) fn require_screenshot_capability(
    app_id: &str,
    entry: &RegisteredApp,
) -> Result<(), FrameSourceError> {
    if entry
        .app
        .capabilities
        .iter()
        .any(|c| c == SCREENSHOT_CAPABILITY)
    {
        Ok(())
    } else {
        Err(FrameSourceError::Unsupported {
            target: app_id.to_string(),
            transport: "websocket",
            reason: WEBSOCKET_NO_CAPABILITY_REASON.to_string(),
        })
    }
}

/// The provider for a live registered app, chosen by the app's TRANSPORT.
/// Pure (no I/O), so the routing is unit-testable without an `ApiState`.
pub(crate) fn provider_for_app(
    app_id: &str,
    entry: &RegisteredApp,
) -> Result<Box<dyn FrameProvider>, FrameSourceError> {
    match entry.transport {
        AppTransport::Http => Ok(Box::new(DeviceScreenshotSource::for_app(app_id, entry))),
        AppTransport::Websocket => {
            require_screenshot_capability(app_id, entry)?;
            Ok(Box::new(WsRelayScreenshotSource {
                app_id: app_id.to_string(),
            }))
        }
    }
}

/// Resolve a [`FrameProvider`] for an optional vision `target`.
///
/// `None` reproduces today's behavior exactly ([`RunnerWindowSource`]). When a
/// target id is supplied it is matched, in priority order, against:
///   1. a registered physical device (its active proxy url),
///   2. a live registered app, by its transport ([`provider_for_app`]),
///   3. an adb serial / emulator transport id.
///
/// Anything else is rejected with [`FrameSourceError::UnknownTarget`].
pub async fn resolve_frame_provider(
    state: &Arc<ApiState>,
    target: &Option<String>,
) -> Result<Box<dyn FrameProvider>, FrameSourceError> {
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
        return Ok(Box::new(DeviceScreenshotSource {
            base_url,
            phone: true,
            target: id.clone(),
        }));
    }

    // 2. Registered app → by its transport.
    if let Some(entry) = state.app_registry.get_live(id).await {
        return provider_for_app(id, &entry);
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

    Err(FrameSourceError::UnknownTarget(id.clone()))
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
    pub(super) fn png_b64(w: u32, h: u32) -> String {
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

    // -----------------------------------------------------------------------
    // Transport-aware app arm + typed errors (plan
    // 2026-11-21-vision-capture-cannot-frame-a-websocket-transport-app)
    // -----------------------------------------------------------------------

    /// The live registry row `app_id` gets when registered over `transport`
    /// advertising `capabilities`.
    async fn registered(
        app_id: &str,
        transport: AppTransport,
        capabilities: &[&str],
    ) -> RegisteredApp {
        let registry = crate::mcp::app_registry::AppRegistry::new();
        let mut app =
            crate::mcp::app_dispatch::test_support::sample_app(app_id, "https://page.example");
        app.capabilities = capabilities.iter().map(|c| c.to_string()).collect();
        let conn = (transport == AppTransport::Websocket).then_some(1);
        registry.upsert(app, None, transport, conn, None).await;
        registry.get_live(app_id).await.expect("live row")
    }

    /// (b) A WebSocket app that does not advertise `screenshot` is REFUSED by
    /// name with 422 `FRAME_SOURCE_UNSUPPORTED` — never routed to an HTTP GET
    /// of its page origin, and never sent a command frame.
    #[tokio::test]
    async fn a_websocket_app_without_the_capability_is_refused_with_422() {
        // One registry, one WS manager, one dispatcher over both — the wiring
        // `create_router` gives `ApiState` — so the frame channel read below is
        // the one a dispatch to this app would write.
        let registry = crate::mcp::app_registry::AppRegistry::new();
        let ws = crate::mcp::ws_relay::WsConnectionManager::new();
        let (conn, mut outbound_rx) = ws.test_register("plain-tab").await;
        let mut app =
            crate::mcp::app_dispatch::test_support::sample_app("plain-tab", "https://page.example");
        app.capabilities = vec!["control".into()];
        registry
            .upsert(app, None, AppTransport::Websocket, Some(conn), None)
            .await;
        let relay = crate::mcp::command_relay::CommandRelay::with_timeout(
            ws,
            std::time::Duration::from_secs(2),
        );
        let dispatcher = crate::mcp::app_dispatch::AppDispatcher::new(registry.clone(), relay);
        let entry = registry.get_live("plain-tab").await.expect("live row");

        let err = match provider_for_app("plain-tab", &entry) {
            Ok(p) => panic!(
                "a capability-less WebSocket app must be refused, got the `{}` source",
                p.source_label()
            ),
            Err(e) => e,
        };
        assert!(
            matches!(&err, FrameSourceError::Unsupported { target, transport: "websocket", .. }
                if target == "plain-tab"),
            "{err:?}"
        );
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code(), "FRAME_SOURCE_UNSUPPORTED");
        assert!(err.to_string().contains("`screenshot` capability"), "{err}");
        let (status, body) = err.to_json_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body.0.code.as_deref(), Some("FRAME_SOURCE_UNSUPPORTED"));

        // The capture path itself refuses too, through the dispatcher wired to
        // this WS manager — and nothing reaches the app's socket.
        let err = ws_relay_source::capture_over_relay(&dispatcher, &registry, "plain-tab")
            .await
            .expect_err("the capture path must refuse as well");
        assert_eq!(err.code(), "FRAME_SOURCE_UNSUPPORTED", "{err:?}");
        assert!(
            outbound_rx.try_recv().is_err(),
            "a refused target must not be sent a frame"
        );
    }

    /// A capable WebSocket app gets the relay source.
    #[tokio::test]
    async fn a_capable_websocket_app_gets_the_relay_source() {
        let entry = registered("wrapper", AppTransport::Websocket, &[SCREENSHOT_CAPABILITY]).await;
        let provider = provider_for_app("wrapper", &entry).expect("capable app resolves");
        assert_eq!(provider.source_label(), "websocket_relay");
    }

    /// (c) An HTTP app still gets `DeviceScreenshotSource`, whatever it advertises.
    #[tokio::test]
    async fn an_http_app_still_gets_the_device_http_source() {
        for caps in [&[][..], &[SCREENSHOT_CAPABILITY][..]] {
            let entry = registered("happ", AppTransport::Http, caps).await;
            let provider = provider_for_app("happ", &entry).expect("http app resolves");
            assert_eq!(
                provider.source_label(),
                "device_http",
                "capabilities {caps:?}"
            );
        }
    }

    /// The HTTP arm's 404 for a non-phone app names the cause (finish-to-zero).
    #[tokio::test]
    async fn an_http_app_that_404s_the_route_is_refused_naming_the_cause() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, axum::Router::new()).await;
        });
        let err = DeviceScreenshotSource {
            base_url: base.clone(),
            phone: false,
            target: "page-app".into(),
        }
        .fetch()
        .await
        .expect_err("a 404 is not a frame");
        assert_eq!(err.code(), "FRAME_SOURCE_UNSUPPORTED", "{err:?}");
        let text = err.to_string();
        assert!(
            text.contains(&format!(
                "app 'page-app' at {base} does not serve /ui-bridge/control/screenshot"
            )),
            "{text}"
        );
        assert!(text.contains("screenshotProvider"), "{text}");
    }

    /// A device screenshot server answering every request with `status` and
    /// `body` (`None` = an empty body). Returns its base url.
    async fn answering(status: axum::http::StatusCode, body: Option<serde_json::Value>) -> String {
        let app = axum::Router::new().route(
            "/ui-bridge/control/screenshot",
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    use axum::response::IntoResponse;
                    match body {
                        Some(b) => (status, axum::Json(b)).into_response(),
                        None => status.into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        base
    }

    async fn fetch_from(base: String) -> FrameSourceError {
        DeviceScreenshotSource {
            base_url: base,
            phone: false,
            target: "proxied".into(),
        }
        .fetch()
        .await
        .expect_err("an error status is not a frame")
    }

    /// A bare gateway status (502/503/504, no UI Bridge envelope) is the
    /// transport's failure; a bare 500 stays the app's failed capture.
    #[tokio::test]
    async fn a_bare_gateway_status_from_the_device_is_unreachable() {
        use axum::http::StatusCode as S;
        for status in [S::BAD_GATEWAY, S::SERVICE_UNAVAILABLE, S::GATEWAY_TIMEOUT] {
            let err = fetch_from(answering(status, None).await).await;
            assert_eq!(err.code(), "APP_UNREACHABLE", "{status}: {err:?}");
            assert_eq!(
                err.to_json_err().1 .0.hint.expect("hint")["failure_origin"],
                "transport",
                "{status}"
            );
        }
        let err = fetch_from(answering(S::INTERNAL_SERVER_ERROR, None).await).await;
        assert_eq!(err.code(), "APP_CAPTURE_FAILED", "{err:?}");
        assert_eq!(
            err.to_json_err().1 .0.hint.expect("hint")["failure_origin"],
            "app"
        );
    }

    /// The native device's OWN 503 refusal (`PAIRING_UNAVAILABLE` envelope)
    /// is a reached device refusing — the app's failure, code kept.
    #[tokio::test]
    async fn a_503_carrying_a_ui_bridge_code_stays_the_apps_refusal() {
        let body = serde_json::json!({
            "success": false,
            "error": "The pairing store is not available yet. Try again.",
            "code": "PAIRING_UNAVAILABLE",
        });
        let err =
            fetch_from(answering(axum::http::StatusCode::SERVICE_UNAVAILABLE, Some(body)).await)
                .await;
        assert_eq!(err.code(), "PAIRING_UNAVAILABLE", "{err:?}");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            err.to_json_err().1 .0.hint.expect("hint")["failure_origin"],
            "app"
        );
        assert!(err.to_string().contains("pairing store"), "{err}");
    }

    #[test]
    fn every_error_class_has_its_status_and_code() {
        let unknown = FrameSourceError::UnknownTarget("x".into());
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        assert_eq!(unknown.code(), "UNKNOWN_VISION_TARGET");
        assert_eq!(unknown.to_string(), "unknown vision target 'x'");
        assert_eq!(unknown.to_json_err().1 .0.hint, None);
        let runner = FrameSourceError::RunnerCapture("no window".into());
        assert_eq!(runner.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            runner.to_string(),
            "no window",
            "the capture text is unchanged"
        );
        assert_eq!(runner.to_frame_error(), "RUNNER_CAPTURE_FAILED: no window");
    }

    /// The providers listed here name exactly the entries of
    /// `VISION_FRAME_SOURCES`, in order. This catches drift between the const
    /// and the hand-listed providers below; a NEW provider type is caught only
    /// if it is added to this list too.
    #[test]
    fn vision_frame_sources_lists_every_provider_exactly() {
        let labels = [
            RunnerWindowSource.source_label(),
            DeviceScreenshotSource {
                base_url: String::new(),
                phone: false,
                target: String::new(),
            }
            .source_label(),
            AdbScreencapSource {
                serial: String::new(),
            }
            .source_label(),
            WsRelayScreenshotSource {
                app_id: String::new(),
            }
            .source_label(),
        ];
        assert_eq!(labels.as_slice(), VISION_FRAME_SOURCES);
    }

    /// (f) Every vision route resolves its frame through this module's typed
    /// error: seven `resolve_frame_provider(` call sites, none of them still
    /// flattening the failure to a blanket 500.
    #[test]
    fn every_vision_call_site_maps_the_typed_error() {
        let src = include_str!("../vision_routes.rs");
        assert_eq!(src.matches("resolve_frame_provider(").count(), 7);
        assert!(
            !src.contains("INTERNAL_SERVER_ERROR, Json(api_error(e))"),
            "a call site still maps every resolver error to 500"
        );
        // Six Json-returning sites, each mapping both the resolve and the frame.
        assert_eq!(src.matches(".map_err(|e| e.to_json_err())").count(), 12);
        // The soft-fail site carries the code into `frame_error`.
        assert_eq!(src.matches(".to_frame_error()").count(), 1);
    }
}
