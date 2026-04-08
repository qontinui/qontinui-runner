//! `CapturedScreenshot` — the structural fix for the dimension bug.
//!
//! No qontinui Python equivalent (the Python code suffers from the same
//! bug pattern: `PhysicalScreen.capture` scales logical region coords to
//! physical without recording which space the result is in). This struct
//! is the runner's answer to "how do I make it impossible to claim the
//! image is one size while the bytes are another?"
//!
//! Every constructor reads dimensions from the actual decoded image. The
//! `debug_assert_consistent` method panics in debug builds if the fields
//! drift from the image — call it after any operation that might mutate
//! the image (resize, crop, format conversion).

use super::monitor::{Monitor, MonitorManager};
use image::DynamicImage;

/// Tag indicating which coordinate space dimensions / bounds are in.
///
/// Almost everywhere in the runner we want `Physical` (the actual image
/// pixels). `Logical` is only useful when reporting back to a CSS / DOM
/// consumer that has not had a chance to scale yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelSpace {
    Physical,
    Logical,
}

/// A screenshot whose stored metadata is *guaranteed* to match the actual
/// image bytes.
#[derive(Debug, Clone)]
pub struct CapturedScreenshot {
    /// The decoded image. Always physical pixels.
    pub image: DynamicImage,
    /// Always equal to `image.width()`.
    pub physical_width: u32,
    /// Always equal to `image.height()`.
    pub physical_height: u32,
    /// Device pixel ratio of the source monitor (physical / logical).
    pub scale_factor: f64,
    /// Index into the [`MonitorManager`] this was captured from, if any.
    pub monitor_index: Option<usize>,
    /// Logical (virtual-desktop) origin of the source monitor at the
    /// moment of capture. `(0, 0)` for the primary monitor.
    pub virtual_desktop_origin: (i32, i32),
}

impl CapturedScreenshot {
    /// Capture a single physical monitor.
    pub fn from_monitor(mgr: &MonitorManager, index: usize) -> Result<Self, String> {
        let monitor = mgr
            .get(index)
            .ok_or_else(|| format!("monitor index {} out of range", index))?;
        let image = monitor.capture()?;
        let physical_width = image.width();
        let physical_height = image.height();

        let captured = Self {
            image,
            physical_width,
            physical_height,
            scale_factor: monitor.scale_factor,
            monitor_index: Some(index),
            virtual_desktop_origin: (monitor.logical_x, monitor.logical_y),
        };
        captured.debug_assert_consistent();
        Ok(captured)
    }

    /// Capture the primary monitor.
    pub fn from_primary(mgr: &MonitorManager) -> Result<Self, String> {
        Self::from_monitor(mgr, mgr.primary_index())
    }

    /// Wrap an already-decoded image with explicit metadata. Use this
    /// when you have an image from a source other than the [`MonitorManager`]
    /// (e.g. cropped from a window capture). The fields are validated
    /// against the image dimensions in debug builds.
    pub fn from_image(
        image: DynamicImage,
        scale_factor: f64,
        monitor_index: Option<usize>,
        virtual_desktop_origin: (i32, i32),
    ) -> Self {
        let physical_width = image.width();
        let physical_height = image.height();
        let captured = Self {
            image,
            physical_width,
            physical_height,
            scale_factor,
            monitor_index,
            virtual_desktop_origin,
        };
        captured.debug_assert_consistent();
        captured
    }

    pub fn logical_width(&self) -> i32 {
        (self.physical_width as f64 / self.scale_factor).round() as i32
    }

    pub fn logical_height(&self) -> i32 {
        (self.physical_height as f64 / self.scale_factor).round() as i32
    }

    /// Panics in debug builds if the recorded dimensions disagree with the
    /// actual image dimensions. In release builds, logs a warning.
    pub fn debug_assert_consistent(&self) {
        let actual_w = self.image.width();
        let actual_h = self.image.height();
        if actual_w != self.physical_width || actual_h != self.physical_height {
            #[cfg(debug_assertions)]
            panic!(
                "CapturedScreenshot dimensions drifted: image is {}x{} but fields say {}x{}",
                actual_w, actual_h, self.physical_width, self.physical_height
            );
            #[cfg(not(debug_assertions))]
            tracing::error!(
                "CapturedScreenshot dimensions drifted: image is {}x{} but fields say {}x{}",
                actual_w,
                actual_h,
                self.physical_width,
                self.physical_height
            );
        }
    }

    /// Encode the image as PNG bytes.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut buf = std::io::Cursor::new(Vec::new());
        self.image
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| format!("PNG encode: {e}"))?;
        Ok(buf.into_inner())
    }

    /// Encode the image as base64 PNG (for IPC / HTTP transport).
    pub fn to_png_base64(&self) -> Result<String, String> {
        use base64::Engine;
        let bytes = self.to_png()?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// Borrow the underlying [`Monitor`] from the manager, if known.
    pub fn monitor<'a>(&self, mgr: &'a MonitorManager) -> Option<&'a Monitor> {
        self.monitor_index.and_then(|i| mgr.get(i))
    }
}
