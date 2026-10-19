//! Reading markers off captured pixels — `marker-golden.json` `codec.pixel`,
//! implemented exactly.
//!
//! The nine pixel rules (the golden file's `codec.pixel.rules`, reference
//! implementation `scripts/termhost-bench/marker-raster.mjs`) turn one pixel
//! row into marker cells: classify each channel into 0, 1 or impure by the
//! golden thresholds; split the row into maximal runs of one pure value; start
//! a candidate at a red run followed by cyan; walk the runs and gaps measuring
//! each in running-mean cell widths; and hand the 20 cells to the cell checks in
//! `decode_order`. [`row`] is that algorithm, line for line; [`scan`] applies it
//! to every row of a frame and finds each marker's rectangle; [`synth`] draws
//! the synthetic rasters the tests prove it against.

pub mod row;
pub mod scan;
pub mod synth;

pub use row::{find_markers_in_row, RowMarker};
pub use scan::{scan, FoundMarker, Rect, RejectedCandidate, ScanOptions, ScanResult};

use crate::spec::{spec, CodecSpec, PixelParams};

/// The decoder's parameters: the golden file's `codec.pixel`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecodeParams {
    pub pixel: PixelParams,
}

impl DecodeParams {
    pub fn from_spec(s: &CodecSpec) -> Self {
        DecodeParams { pixel: s.pixel }
    }
}

impl Default for DecodeParams {
    fn default() -> Self {
        DecodeParams::from_spec(spec())
    }
}

/// Byte order of a 4-byte pixel. Alpha / padding bytes are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// B, G, R, A — DXGI `B8G8R8A8`, WGC, and an LSB-first 24-bit X11 ZPixmap.
    Bgra8,
    /// R, G, B, A.
    Rgba8,
    /// A (or padding), R, G, B — an MSB-first 24-bit X11 ZPixmap.
    Argb8,
}

impl PixelFormat {
    #[inline]
    fn rgb(self, px: &[u8]) -> [u8; 3] {
        match self {
            PixelFormat::Bgra8 => [px[2], px[1], px[0]],
            PixelFormat::Rgba8 => [px[0], px[1], px[2]],
            PixelFormat::Argb8 => [px[1], px[2], px[3]],
        }
    }
}

/// Why a buffer cannot be viewed as a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameError(pub String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FrameError {}

/// A borrowed 4-byte-per-pixel image. Raw frames are processed in memory and
/// never written anywhere (plan Phase 2: a desktop capture can contain other
/// windows).
#[derive(Clone, Copy, Debug)]
pub struct FrameView<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    stride: usize,
    format: PixelFormat,
}

impl<'a> FrameView<'a> {
    /// View `data` as `width` x `height` pixels, `stride` bytes per row.
    pub fn new(
        data: &'a [u8],
        width: usize,
        height: usize,
        stride: usize,
        format: PixelFormat,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError(format!("empty frame {width}x{height}")));
        }
        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| FrameError("frame width overflows".into()))?;
        if stride < row_bytes {
            return Err(FrameError(format!(
                "stride {stride} is below the row size {row_bytes}"
            )));
        }
        let needed = stride
            .checked_mul(height - 1)
            .and_then(|n| n.checked_add(row_bytes))
            .ok_or_else(|| FrameError("frame size overflows".into()))?;
        if data.len() < needed {
            return Err(FrameError(format!(
                "buffer of {} bytes is smaller than {width}x{height} at stride {stride} ({needed})",
                data.len()
            )));
        }
        Ok(FrameView {
            data,
            width,
            height,
            stride,
            format,
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The RGB of pixel (x, y). Panics outside the frame.
    #[inline]
    pub fn rgb(&self, x: usize, y: usize) -> [u8; 3] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) outside the frame"
        );
        let at = y * self.stride + x * 4;
        self.format.rgb(&self.data[at..at + 4])
    }

    /// Classify pixels `x0..x1` of row `y` into `out` (cleared first).
    pub fn classify_span(
        &self,
        y: usize,
        x0: usize,
        x1: usize,
        p: &PixelParams,
        out: &mut Vec<Class>,
    ) {
        out.clear();
        let x1 = x1.min(self.width);
        if x0 >= x1 {
            return;
        }
        let start = y * self.stride + x0 * 4;
        let row = &self.data[start..start + (x1 - x0) * 4];
        out.extend(row.chunks_exact(4).map(|px| {
            let [r, g, b] = self.format.rgb(px);
            classify(p, r, g, b)
        }));
    }

    /// Classify row `y` into `out` (cleared first).
    pub fn classify_row(&self, y: usize, p: &PixelParams, out: &mut Vec<Class>) {
        self.classify_span(y, 0, self.width, p, out);
    }

    /// A sub-rectangle view (clipped to the frame); `None` when it is empty.
    pub fn sub(&self, x: usize, y: usize, width: usize, height: usize) -> Option<FrameView<'a>> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);
        if width == 0 || height == 0 {
            return None;
        }
        let start = y * self.stride + x * 4;
        Some(FrameView {
            data: &self.data[start..],
            width,
            height,
            stride: self.stride,
            format: self.format,
        })
    }
}

/// A pixel's class (rule 2): one of the 8 palette values, or impure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Pure(u8),
    Impure,
}

#[inline]
fn channel_bit(p: &PixelParams, c: u8) -> Option<u8> {
    if c <= p.channel_low_max {
        Some(0)
    } else if c >= p.channel_high_min {
        Some(1)
    } else {
        None
    }
}

/// Rule 2: a channel <= `channel_low_max` is 0, >= `channel_high_min` is 1,
/// anything else is impure; a pixel is pure when all three channels are, with
/// value 4R + 2G + B.
#[inline]
pub fn classify(p: &PixelParams, r: u8, g: u8, b: u8) -> Class {
    match (channel_bit(p, r), channel_bit(p, g), channel_bit(p, b)) {
        (Some(r), Some(g), Some(b)) => Class::Pure(4 * r + 2 * g + b),
        _ => Class::Impure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{xterm_rgb, PALETTE};

    fn golden() -> PixelParams {
        spec().pixel
    }

    #[test]
    fn palette_colours_classify_to_their_values() {
        for p in PALETTE {
            let [r, g, b] = p.rgb;
            assert_eq!(
                classify(&golden(), r, g, b),
                Class::Pure(p.value),
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn the_bands_are_inclusive_and_their_neighbours_impure() {
        let p = golden();
        assert_eq!(classify(&p, 63, 192, 63), Class::Pure(2));
        assert_eq!(classify(&p, 64, 255, 0), Class::Impure);
        assert_eq!(classify(&p, 191, 0, 0), Class::Impure);
    }

    #[test]
    fn every_off_palette_cube_level_is_impure() {
        // 95, 135 and 175 lie in the impure band; so does 215 (> 192? no — 215 is
        // a full channel) — the assertion is per level, below.
        let p = golden();
        for index in 16u8..=231 {
            let rgb = xterm_rgb(index).expect("cube index");
            let class = classify(&p, rgb[0], rgb[1], rgb[2]);
            if PALETTE.iter().any(|q| q.index == index) {
                assert!(matches!(class, Class::Pure(_)));
            } else if rgb
                .iter()
                .any(|&c| c > p.channel_low_max && c < p.channel_high_min)
            {
                assert_eq!(class, Class::Impure, "index {index} rgb {rgb:?}");
            }
        }
        // The golden off-palette vector's level.
        assert_eq!(classify(&p, 135, 0, 0), Class::Impure);
    }

    #[test]
    fn frame_view_validates_and_reads_formats() {
        let data = [10u8, 20, 30, 255, 1, 2, 3, 4];
        let v = FrameView::new(&data, 2, 1, 8, PixelFormat::Bgra8).expect("view");
        assert_eq!(v.rgb(0, 0), [30, 20, 10]);
        let v = FrameView::new(&data, 2, 1, 8, PixelFormat::Rgba8).expect("view");
        assert_eq!(v.rgb(1, 0), [1, 2, 3]);
        let v = FrameView::new(&data, 2, 1, 8, PixelFormat::Argb8).expect("view");
        assert_eq!(v.rgb(1, 0), [2, 3, 4]);
        assert!(FrameView::new(&data, 3, 1, 12, PixelFormat::Bgra8).is_err());
        assert!(FrameView::new(&data, 2, 1, 4, PixelFormat::Bgra8).is_err());
    }

    #[test]
    fn sub_views_clip_and_offset() {
        let mut data = vec![0u8; 4 * 4 * 4];
        let at = (2 * 4 + 3) * 4;
        data[at..at + 3].copy_from_slice(&[9, 8, 7]);
        let v = FrameView::new(&data, 4, 4, 16, PixelFormat::Bgra8).expect("view");
        let s = v.sub(2, 1, 10, 10).expect("sub");
        assert_eq!((s.width(), s.height()), (2, 3));
        assert_eq!(s.rgb(1, 1), [7, 8, 9]);
        assert!(v.sub(4, 0, 1, 1).is_none());
    }
}
