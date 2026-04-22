//! VGA (Visual GUI Automation) HTTP endpoints.
//!
//! Thin HTTP surface serving the state-machine-builder UI in `qontinui-web`
//! and the `vga_automate` runner step handler. Reuses the existing
//! cross-platform capture model in [`crate::screen`] (xcap-backed; the Rust
//! peer of the qontinui Python `UnifiedCaptureService`). See
//! `plans/vga-product-surface-2026-04-21.md` §13 "Builder screenshot input".
//!
//! Routes:
//!
//! - `GET /vga/capture?monitor=N&region=x,y,w,h` — PNG bytes of a monitor
//!   (or a region of it).
//! - `GET /vga/monitors` — monitor enumeration for the builder UI.
//!
//! The router is mounted from [`crate::mcp_api`] alongside `/ui-bridge/*`
//! and inherits the permissive CORS layer applied there.

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::ApiState;
use crate::screen::{CapturedScreenshot, MonitorManager};

// ── Query types ─────────────────────────────────────────────────────────────

/// Query parameters for `GET /vga/capture`.
#[derive(Debug, Deserialize)]
pub struct CaptureQuery {
    /// Monitor index (0-based). Defaults to the primary monitor.
    pub monitor: Option<usize>,
    /// Optional crop region in **physical pixels**, monitor-local, as
    /// `"x,y,w,h"`. If omitted, the full monitor is returned.
    pub region: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MonitorInfo {
    pub id: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub is_primary: bool,
    pub scale_factor: f64,
}

#[derive(Debug, Serialize)]
pub struct MonitorsResponse {
    pub monitors: Vec<MonitorInfo>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /vga/capture` — PNG screenshot of a monitor (optionally cropped).
pub async fn get_capture(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<CaptureQuery>,
) -> Response {
    let monitor = query.monitor;
    let region_str = query.region.clone();

    // Parse region on the async side so we can return 400 before the blocking
    // capture thread is spawned.
    let region = match parse_region(region_str.as_deref()) {
        Ok(r) => r,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, &msg),
    };

    // xcap capture must run on a blocking thread (Windows COM/GDI).
    let result = tokio::task::spawn_blocking(move || capture_png(monitor, region)).await;

    match result {
        Ok(Ok(png_bytes)) => {
            info!(
                monitor = ?monitor,
                region = ?region,
                bytes = png_bytes.len(),
                "vga capture"
            );
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "image/png")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(png_bytes))
                .unwrap_or_else(|_| {
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to build capture response",
                    )
                })
        }
        Ok(Err(e)) => {
            error!(error = %e, "vga capture failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
        Err(e) => {
            error!(error = %e, "vga capture task panicked");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "capture task panicked")
        }
    }
}

/// `GET /vga/monitors` — monitor enumeration as JSON.
pub async fn get_monitors(State(_state): State<Arc<ApiState>>) -> Response {
    let result = tokio::task::spawn_blocking(enumerate_monitors).await;

    match result {
        Ok(Ok(resp)) => (StatusCode::OK, Json(resp)).into_response(),
        Ok(Err(e)) => {
            error!(error = %e, "vga monitor enumeration failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
        Err(e) => {
            error!(error = %e, "vga monitor enumeration task panicked");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "monitor enumeration task panicked",
            )
        }
    }
}

// ── Router ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/vga/capture", get(get_capture))
        .route("/vga/monitors", get(get_monitors))
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Parse a `"x,y,w,h"` region string. Returns `Ok(None)` when the input is
/// absent, `Ok(Some(...))` on a valid region, `Err` on a malformed region.
fn parse_region(s: Option<&str>) -> Result<Option<(u32, u32, u32, u32)>, String> {
    let Some(raw) = s else {
        return Ok(None);
    };
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(format!(
            "region must be 'x,y,w,h' (got {} components)",
            parts.len()
        ));
    }
    let parse = |label: &str, v: &str| -> Result<u32, String> {
        v.parse::<i64>()
            .map_err(|_| format!("region {label} not an integer: {v:?}"))
            .and_then(|n| {
                if n < 0 {
                    Err(format!("region {label} must be >= 0 (got {n})"))
                } else if n > u32::MAX as i64 {
                    Err(format!("region {label} out of range"))
                } else {
                    Ok(n as u32)
                }
            })
    };
    let x = parse("x", parts[0])?;
    let y = parse("y", parts[1])?;
    let w = parse("w", parts[2])?;
    let h = parse("h", parts[3])?;
    if w == 0 || h == 0 {
        return Err(format!("region w and h must be > 0 (got w={w}, h={h})"));
    }
    Ok(Some((x, y, w, h)))
}

/// Capture a monitor (optionally cropped to `region`) and return PNG bytes.
/// Runs on a blocking thread — xcap requires it on Windows.
fn capture_png(
    monitor: Option<usize>,
    region: Option<(u32, u32, u32, u32)>,
) -> Result<Vec<u8>, String> {
    let mgr = MonitorManager::detect()?;
    let idx = monitor.unwrap_or_else(|| mgr.primary_index());
    if mgr.get(idx).is_none() {
        return Err(format!(
            "monitor index {idx} out of range (have {} monitors)",
            mgr.all().len()
        ));
    }

    let captured = CapturedScreenshot::from_monitor(&mgr, idx)?;

    match region {
        None => captured.to_png(),
        Some((x, y, w, h)) => crop_to_png(&captured, x, y, w, h),
    }
}

fn crop_to_png(
    captured: &CapturedScreenshot,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    let img_w = captured.physical_width;
    let img_h = captured.physical_height;
    if x >= img_w || y >= img_h {
        return Err(format!(
            "region origin ({x},{y}) outside monitor bounds ({img_w}x{img_h})"
        ));
    }
    if x.saturating_add(w) > img_w || y.saturating_add(h) > img_h {
        return Err(format!(
            "region {x},{y},{w},{h} exceeds monitor bounds ({img_w}x{img_h})"
        ));
    }
    let cropped = captured.image.crop_imm(x, y, w, h);
    let mut buf = std::io::Cursor::new(Vec::new());
    cropped
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(buf.into_inner())
}

fn enumerate_monitors() -> Result<MonitorsResponse, String> {
    let mgr = MonitorManager::detect()?;
    let monitors = mgr
        .all()
        .iter()
        .map(|m| MonitorInfo {
            id: m.index,
            name: m.name.clone(),
            width: m.physical_width,
            height: m.physical_height,
            x: m.logical_x,
            y: m.logical_y,
            is_primary: m.is_primary,
            scale_factor: m.scale_factor,
        })
        .collect();
    Ok(MonitorsResponse { monitors })
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(r#"{"error":"response build failed"}"#))
                .unwrap()
        })
}
