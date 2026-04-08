//! Cross-platform monitor / DPI / capture model.
//!
//! Wraps `xcap` with a qontinui-style API. The conceptual model is ported
//! from the qontinui Python library
//! (`qontinui/hal/implementations/mss_capture.py:1-45`), adapted for the
//! semantics that `xcap` actually exposes.
//!
//! # Coordinate spaces
//!
//! There are three coordinate systems and they are NOT interchangeable. Most
//! of the bugs we have hit on HiDPI multi-monitor rigs come from confusing
//! them.
//!
//! 1. **Logical / CSS pixels.** What the DOM, Tauri's
//!    `LogicalPosition`, the OS window manager, and `xcap::Monitor::x()` /
//!    `Monitor::y()` all report. Equal to physical pixels divided by the
//!    monitor's DPI scale factor.
//!
//! 2. **Physical pixels.** Pixels in the actual captured image bytes.
//!    `xcap::Monitor::width()` / `Monitor::height()` return *physical*
//!    pixels (specifically `dmPelsWidth` / `dmPelsHeight` on Windows).
//!    `xcap::Monitor::capture_image()` returns an image whose dimensions
//!    are also in physical pixels.
//!
//! 3. **Virtual desktop (multi-monitor) coordinates.** A single coordinate
//!    space spanning every connected monitor. The origin is `(0, 0)` at
//!    the top-left of the *primary* monitor; monitors above or to the left
//!    of the primary therefore have **negative** logical x / y coordinates.
//!    The full virtual desktop bounding box may extend from e.g.
//!    `(-1920, -120)` to `(3840, 2160)` on a 3-monitor rig.
//!
//! # The original bug this module exists to prevent
//!
//! `sm_capture_screenshots` was storing `width` / `height` columns that
//! were `physical_px * device_pixel_ratio` — i.e. 1.5x the actual stored
//! WebP bytes on a 1.5-DPR display. The frontend was multiplying physical
//! values by DPR a second time before persisting them. The Rust writer
//! never cross-checked them against the decoded image. With this module,
//! the only way to construct a `CapturedScreenshot` is via
//! [`CapturedScreenshot::from_monitor`] or [`CapturedScreenshot::from_image`],
//! both of which read the dimensions from the image bytes themselves and
//! call [`CapturedScreenshot::debug_assert_consistent`] in debug builds.
//!
//! # What we explicitly do NOT do
//!
//! - We do not implement template matching, OCR, or any vision algorithms.
//!   `imageproc` already provides those primitives where the runner needs
//!   them.
//! - We do not implement multi-backend capture (qontinui Python's
//!   `UnifiedCaptureService` registry). xcap is the only Rust backend and
//!   has no peer.
//! - We do not implement screenshot caching. Caches belong at the call
//!   site if they are needed at all.

pub mod captured;
pub mod dpi;
pub mod monitor;

pub use captured::{CapturedScreenshot, PixelSpace};
pub use dpi::ensure_dpi_awareness;
pub use monitor::{Monitor, MonitorManager};
