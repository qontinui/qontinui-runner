//! Monitor enumeration and coordinate translation.
//!
//! Modeled on `qontinui/monitor/monitor_manager.py` (`MonitorInfo` /
//! `MonitorManager`) and `qontinui/hal/interfaces/screen_capture.py`
//! (`Monitor` HAL dataclass), wrapping `xcap::Monitor`. The Python concepts
//! we kept:
//!
//! - `MonitorInfo` struct fields (index, x, y, width, height, scale,
//!   is_primary)
//! - `contains_point` hit-test
//! - `to_monitor_local` / `to_global` coordinate translation
//! - Closest-to-(0,0) primary detection (only as a fallback when xcap
//!   doesn't surface a primary)
//!
//! Concepts we did NOT port:
//! - Per-operation monitor assignment map — runner has no equivalent
//!   feature.
//! - tkinter / PIL fallback paths — irrelevant in Rust.
//! - Virtual-desktop-only enumeration mode — xcap only enumerates physical
//!   monitors.

use xcap::Monitor as XcapMonitor;

/// A single physical monitor.
#[derive(Debug, Clone)]
pub struct Monitor {
    /// Zero-based runner index.
    pub index: usize,
    /// Logical (CSS) x of the monitor's top-left corner in virtual-desktop
    /// space. Can be negative if the monitor is to the left of the primary.
    pub logical_x: i32,
    /// Logical (CSS) y of the monitor's top-left corner. Can be negative.
    pub logical_y: i32,
    /// Image-pixel width (`dmPelsWidth` on Windows). What you get from
    /// `capture_image()`.
    pub physical_width: u32,
    /// Image-pixel height.
    pub physical_height: u32,
    /// physical / logical (e.g. 1.0, 1.25, 1.5, 2.0).
    pub scale_factor: f64,
    pub is_primary: bool,
    pub name: String,
}

impl Monitor {
    pub fn logical_width(&self) -> i32 {
        (self.physical_width as f64 / self.scale_factor).round() as i32
    }

    pub fn logical_height(&self) -> i32 {
        (self.physical_height as f64 / self.scale_factor).round() as i32
    }

    /// Logical bounding box in virtual-desktop coordinates: `(x, y, w, h)`.
    pub fn logical_bounds(&self) -> (i32, i32, i32, i32) {
        (
            self.logical_x,
            self.logical_y,
            self.logical_width(),
            self.logical_height(),
        )
    }

    /// True if `(x, y)` lies inside this monitor's logical bounding box.
    pub fn contains_logical_point(&self, x: i32, y: i32) -> bool {
        let (mx, my, mw, mh) = self.logical_bounds();
        x >= mx && x < mx + mw && y >= my && y < my + mh
    }

    /// Convert a virtual-desktop logical coordinate to a monitor-local
    /// logical coordinate (origin at this monitor's top-left).
    pub fn to_monitor_local(&self, global_x: i32, global_y: i32) -> (i32, i32) {
        (global_x - self.logical_x, global_y - self.logical_y)
    }

    /// Convert a monitor-local logical coordinate back to virtual-desktop
    /// space.
    pub fn to_global(&self, local_x: i32, local_y: i32) -> (i32, i32) {
        (local_x + self.logical_x, local_y + self.logical_y)
    }

    /// Convert a monitor-local logical coordinate to physical pixels
    /// inside the captured image (origin at the captured image's top-left).
    pub fn logical_to_physical(&self, local_x: i32, local_y: i32) -> (i32, i32) {
        (
            (local_x as f64 * self.scale_factor).round() as i32,
            (local_y as f64 * self.scale_factor).round() as i32,
        )
    }

    /// Convert a physical pixel coordinate inside the captured image to a
    /// monitor-local logical coordinate.
    pub fn physical_to_logical(&self, phys_x: i32, phys_y: i32) -> (i32, i32) {
        (
            (phys_x as f64 / self.scale_factor).round() as i32,
            (phys_y as f64 / self.scale_factor).round() as i32,
        )
    }

    fn from_xcap(index: usize, m: &XcapMonitor) -> Result<Self, String> {
        let logical_x = m.x().map_err(|e| format!("xcap monitor x: {e}"))?;
        let logical_y = m.y().map_err(|e| format!("xcap monitor y: {e}"))?;
        let physical_width = m.width().map_err(|e| format!("xcap monitor width: {e}"))?;
        let physical_height = m
            .height()
            .map_err(|e| format!("xcap monitor height: {e}"))?;
        let scale_factor = m
            .scale_factor()
            .map_err(|e| format!("xcap monitor scale: {e}"))? as f64;
        let is_primary = m.is_primary().unwrap_or(false);
        let name = m.name().unwrap_or_default();

        Ok(Self {
            index,
            logical_x,
            logical_y,
            physical_width,
            physical_height,
            scale_factor,
            is_primary,
            name,
        })
    }

    /// Capture this monitor as a fresh image. The returned image is in
    /// physical pixels.
    pub fn capture(&self) -> Result<image::DynamicImage, String> {
        let monitors = XcapMonitor::all().map_err(|e| format!("xcap Monitor::all: {e}"))?;
        let m = monitors
            .get(self.index)
            .ok_or_else(|| format!("monitor index {} no longer exists", self.index))?;
        let img = m
            .capture_image()
            .map_err(|e| format!("xcap capture_image: {e}"))?;
        Ok(image::DynamicImage::ImageRgba8(img))
    }
}

/// Snapshot of every monitor at the time `detect()` was called.
///
/// Modeled on `qontinui/monitor/monitor_manager.py::MonitorManager`. Unlike
/// that class, we do NOT cache forever — call `detect()` whenever you need
/// a fresh view (xcap is fast). The Python class persists per-operation
/// monitor assignments which the runner does not need.
#[derive(Debug, Clone)]
pub struct MonitorManager {
    monitors: Vec<Monitor>,
    primary_index: usize,
}

impl MonitorManager {
    /// Enumerate all attached physical monitors via xcap.
    pub fn detect() -> Result<Self, String> {
        let xcap_monitors = XcapMonitor::all().map_err(|e| format!("xcap Monitor::all: {e}"))?;
        if xcap_monitors.is_empty() {
            return Err("xcap returned zero monitors".to_string());
        }

        let monitors: Vec<Monitor> = xcap_monitors
            .iter()
            .enumerate()
            .map(|(i, m)| Monitor::from_xcap(i, m))
            .collect::<Result<_, _>>()?;

        // Prefer xcap's `is_primary`. Fall back to the qontinui Python
        // heuristic (closest to origin) only if no monitor is marked
        // primary, e.g. on weird headless / virtual display configs.
        let primary_index = monitors
            .iter()
            .position(|m| m.is_primary)
            .or_else(|| {
                monitors
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, m)| m.logical_x.abs() + m.logical_y.abs())
                    .map(|(i, _)| i)
            })
            .unwrap_or(0);

        Ok(Self {
            monitors,
            primary_index,
        })
    }

    pub fn all(&self) -> &[Monitor] {
        &self.monitors
    }

    pub fn primary(&self) -> &Monitor {
        &self.monitors[self.primary_index]
    }

    pub fn primary_index(&self) -> usize {
        self.primary_index
    }

    pub fn get(&self, index: usize) -> Option<&Monitor> {
        self.monitors.get(index)
    }

    /// Find the monitor whose logical bounding box contains `(x, y)`.
    /// Returns `None` if the point is in the gap between monitors or
    /// outside the virtual desktop entirely.
    pub fn at_logical_point(&self, x: i32, y: i32) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|m| m.contains_logical_point(x, y))
    }

    /// Logical-pixel bounding box of the entire virtual desktop:
    /// `(x_min, y_min, width, height)`. Useful for capture-everything
    /// modes.
    pub fn virtual_desktop_bounds(&self) -> (i32, i32, i32, i32) {
        let mut x_min = i32::MAX;
        let mut y_min = i32::MAX;
        let mut x_max = i32::MIN;
        let mut y_max = i32::MIN;
        for m in &self.monitors {
            let (mx, my, mw, mh) = m.logical_bounds();
            if mx < x_min {
                x_min = mx;
            }
            if my < y_min {
                y_min = my;
            }
            if mx + mw > x_max {
                x_max = mx + mw;
            }
            if my + mh > y_max {
                y_max = my + mh;
            }
        }
        (x_min, y_min, x_max - x_min, y_max - y_min)
    }
}
