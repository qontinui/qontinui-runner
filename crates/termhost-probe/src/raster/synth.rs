//! Synthetic rasters: markers drawn the ways real hosts and compositors draw
//! them, so the decoder can be proven against each distortion without a
//! display.
//!
//! - [`EdgeMode::Exact`]: box-filtered area coverage in sRGB space — what a
//!   DPI-aware renderer produces for cells at fractional positions.
//! - [`EdgeMode::GammaCorrect`]: the same coverage blended in linear light, so
//!   a 50 % edge pixel between red and cyan reads (188,188,188) — bright, just
//!   inside the impure band, and pure for any single-channel edge from about
//!   53 % coverage.
//! - [`EdgeMode::Sequential`]: cells composited one after another with
//!   coverage alpha (the conflation artefact many 2D renderers have).
//! - [`Canvas::resample`]: crisp content scaled by a compositor (DWM stretching
//!   a DPI-unaware window, a scaled capture) with bilinear or bicubic filtering.
//! - [`render_spans`]: `marker-raster.mjs` `renderSpans`, ported exactly — the
//!   renderer the golden raster rows were drawn with — plus the golden file's
//!   run-length row encoding ([`decode_rle`], [`encode_rle`]).

use super::{FrameView, PixelFormat};

/// How cell edges are anti-aliased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeMode {
    Exact,
    GammaCorrect,
    Sequential,
}

/// Resampling filter for [`Canvas::resample`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Bilinear,
    /// Catmull-Rom (Keys, a = -0.5): overshoots at edges, then clamps.
    Bicubic,
}

/// An owned BGRA image.
#[derive(Clone, Debug)]
pub struct Canvas {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

fn srgb_to_linear(c: f64) -> f64 {
    let c = c / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    c * 255.0
}

fn to_u8(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// The length of `[a0, a1) ∩ [b0, b1)`.
fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

impl Canvas {
    /// A `width` x `height` canvas filled with `bg`.
    pub fn new(width: usize, height: usize, bg: [u8; 3]) -> Self {
        let mut data = vec![0u8; width * height * 4];
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&[bg[2], bg[1], bg[0], 255]);
        }
        Canvas {
            width,
            height,
            data,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The raw BGRA bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// A frame view of the canvas.
    pub fn view(&self) -> FrameView<'_> {
        FrameView::new(
            &self.data,
            self.width,
            self.height,
            self.width * 4,
            PixelFormat::Bgra8,
        )
        .expect("a canvas is always a valid frame")
    }

    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * 4;
        [self.data[at + 2], self.data[at + 1], self.data[at]]
    }

    pub fn put(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let at = (y * self.width + x) * 4;
        self.data[at..at + 3].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
    }

    /// Fill an integer rectangle crisply (clipped).
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, rgb: [u8; 3]) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                self.put(xx, yy, rgb);
            }
        }
    }

    /// Draw a row of cells with exact area coverage.
    pub fn draw_marker(&mut self, x: f64, y: f64, cell_w: f64, cell_h: f64, colors: &[[u8; 3]]) {
        self.draw_cells(x, y, cell_w, cell_h, colors, EdgeMode::Exact);
    }

    /// Draw a row of `colors.len()` cells, `cell_w` x `cell_h` each, starting
    /// at (x, y), anti-aliased per `mode`.
    pub fn draw_cells(
        &mut self,
        x: f64,
        y: f64,
        cell_w: f64,
        cell_h: f64,
        colors: &[[u8; 3]],
        mode: EdgeMode,
    ) {
        let n = colors.len() as f64;
        let x_end = x + n * cell_w;
        let y_end = y + cell_h;
        let px_lo = x.floor().max(0.0) as usize;
        let px_hi = (x_end.ceil().max(0.0) as usize).min(self.width);
        let py_lo = y.floor().max(0.0) as usize;
        let py_hi = (y_end.ceil().max(0.0) as usize).min(self.height);
        match mode {
            EdgeMode::Sequential => {
                for (i, &c) in colors.iter().enumerate() {
                    let cx0 = x + i as f64 * cell_w;
                    self.fill_rect_coverage(cx0, y, cx0 + cell_w, y_end, c);
                }
            }
            EdgeMode::Exact | EdgeMode::GammaCorrect => {
                let linear = mode == EdgeMode::GammaCorrect;
                for py in py_lo..py_hi {
                    let vc = overlap(py as f64, py as f64 + 1.0, y, y_end);
                    if vc <= 0.0 {
                        continue;
                    }
                    for px in px_lo..px_hi {
                        let mut acc = [0.0f64; 3];
                        let mut cov = 0.0;
                        for (i, c) in colors.iter().enumerate() {
                            let cx0 = x + i as f64 * cell_w;
                            let hc = overlap(px as f64, px as f64 + 1.0, cx0, cx0 + cell_w);
                            if hc <= 0.0 {
                                continue;
                            }
                            let a = hc * vc;
                            cov += a;
                            for (k, ch) in c.iter().enumerate() {
                                let v = f64::from(*ch);
                                acc[k] += a * if linear { srgb_to_linear(v) } else { v };
                            }
                        }
                        let bg = self.get(px, py);
                        let mut out = [0u8; 3];
                        for k in 0..3 {
                            let b = f64::from(bg[k]);
                            let b = if linear { srgb_to_linear(b) } else { b };
                            let v = acc[k] + (1.0 - cov) * b;
                            out[k] = to_u8(if linear { linear_to_srgb(v) } else { v });
                        }
                        self.put(px, py, out);
                    }
                }
            }
        }
    }

    /// Composite one rectangle over the canvas with coverage alpha.
    fn fill_rect_coverage(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, rgb: [u8; 3]) {
        let px_lo = x0.floor().max(0.0) as usize;
        let px_hi = (x1.ceil().max(0.0) as usize).min(self.width);
        let py_lo = y0.floor().max(0.0) as usize;
        let py_hi = (y1.ceil().max(0.0) as usize).min(self.height);
        for py in py_lo..py_hi {
            let vc = overlap(py as f64, py as f64 + 1.0, y0, y1);
            for px in px_lo..px_hi {
                let a = vc * overlap(px as f64, px as f64 + 1.0, x0, x1);
                if a <= 0.0 {
                    continue;
                }
                let bg = self.get(px, py);
                let mut out = [0u8; 3];
                for k in 0..3 {
                    out[k] = to_u8(a * f64::from(rgb[k]) + (1.0 - a) * f64::from(bg[k]));
                }
                self.put(px, py, out);
            }
        }
    }

    /// Apply a colour transform to every pixel.
    pub fn map(&mut self, mut f: impl FnMut([u8; 3]) -> [u8; 3]) {
        for px in self.data.chunks_exact_mut(4) {
            let [r, g, b] = f([px[2], px[1], px[0]]);
            px[0] = b;
            px[1] = g;
            px[2] = r;
        }
    }

    /// Apply a colour transform inside a rectangle.
    pub fn map_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mut f: impl FnMut([u8; 3]) -> [u8; 3],
    ) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                let c = self.get(xx, yy);
                self.put(xx, yy, f(c));
            }
        }
    }

    /// The canvas scaled by `scale` with `filter` (sRGB-space interpolation,
    /// as compositors do).
    pub fn resample(&self, scale: f64, filter: Filter) -> Canvas {
        let w = ((self.width as f64) * scale).round().max(1.0) as usize;
        let h = ((self.height as f64) * scale).round().max(1.0) as usize;
        let mut out = Canvas::new(w, h, [0, 0, 0]);
        let sample = |x: i64, y: i64| -> [f64; 3] {
            let xx = x.clamp(0, self.width as i64 - 1) as usize;
            let yy = y.clamp(0, self.height as i64 - 1) as usize;
            let c = self.get(xx, yy);
            [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])]
        };
        for oy in 0..h {
            let sy = (oy as f64 + 0.5) / scale - 0.5;
            for ox in 0..w {
                let sx = (ox as f64 + 0.5) / scale - 0.5;
                let rgb = match filter {
                    Filter::Nearest => sample(sx.round() as i64, sy.round() as i64),
                    Filter::Bilinear => {
                        let x0 = sx.floor();
                        let y0 = sy.floor();
                        let fx = sx - x0;
                        let fy = sy - y0;
                        let (x0, y0) = (x0 as i64, y0 as i64);
                        let a = sample(x0, y0);
                        let b = sample(x0 + 1, y0);
                        let c = sample(x0, y0 + 1);
                        let d = sample(x0 + 1, y0 + 1);
                        let mut v = [0.0; 3];
                        for k in 0..3 {
                            v[k] = (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy)
                                + (c[k] * (1.0 - fx) + d[k] * fx) * fy;
                        }
                        v
                    }
                    Filter::Bicubic => {
                        let x0 = sx.floor();
                        let y0 = sy.floor();
                        let wx = cubic_weights(sx - x0);
                        let wy = cubic_weights(sy - y0);
                        let mut v = [0.0; 3];
                        for (j, wyj) in wy.iter().enumerate() {
                            for (i, wxi) in wx.iter().enumerate() {
                                let s = sample(x0 as i64 + i as i64 - 1, y0 as i64 + j as i64 - 1);
                                for k in 0..3 {
                                    v[k] += s[k] * wxi * wyj;
                                }
                            }
                        }
                        v
                    }
                };
                out.put(ox, oy, [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])]);
            }
        }
        out
    }
}

/// One horizontal run of a colour, `width` pixels wide (fractional allowed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub width: f64,
    pub rgb: [u8; 3],
}

impl Span {
    pub fn new(width: f64, rgb: [u8; 3]) -> Self {
        Span { width, rgb }
    }
}

/// `marker-raster.mjs` `renderSpans`, statement for statement: the spans laid
/// out from x = 0; pixel x covers [x - blur, x + 1 + blur) clipped to [0,
/// total) and takes the coverage-weighted mean colour, rounded per channel; the
/// row is round(total) pixels long. The same binary64 operations in the same
/// order, so a row drawn here is byte-identical to the reference's.
///
/// Panics on a span width that is not positive (the reference throws).
pub fn render_spans(spans: &[Span], blur: f64) -> Vec<[u8; 3]> {
    let mut placed: Vec<(f64, f64, [u8; 3])> = Vec::with_capacity(spans.len());
    let mut x = 0.0f64;
    for span in spans {
        assert!(
            span.width > 0.0,
            "span width must be positive: {}",
            span.width
        );
        placed.push((x, x + span.width, span.rgb));
        x += span.width;
    }
    let total = x.round() as usize;
    let mut pixels = Vec::with_capacity(total);
    for px in 0..total {
        let px = px as f64;
        let a = (px - blur).max(0.0);
        let b = x.min(px + 1.0 + blur);
        let mut sum = [0.0f64; 3];
        for &(from, to, rgb) in &placed {
            let overlap = b.min(to) - a.max(from);
            if overlap > 0.0 {
                for c in 0..3 {
                    sum[c] += overlap * f64::from(rgb[c]);
                }
            }
        }
        pixels.push(sum.map(|v| to_u8(v / (b - a))));
    }
    pixels
}

/// The golden file's row encoding: `[[r, g, b, count], ...]` -> pixels.
pub fn decode_rle(rle: &[[u32; 4]]) -> Vec<[u8; 3]> {
    let mut out = Vec::new();
    for &[r, g, b, n] in rle {
        let rgb = [r, g, b].map(|c| u8::try_from(c).expect("an RLE channel is 0..=255"));
        out.extend(std::iter::repeat_n(rgb, n as usize));
    }
    out
}

/// Pixels -> `[[r, g, b, count], ...]` (`marker-raster.mjs` `encodeRle`).
pub fn encode_rle(pixels: &[[u8; 3]]) -> Vec<[u32; 4]> {
    let mut out: Vec<[u32; 4]> = Vec::new();
    for &[r, g, b] in pixels {
        let rgb = [u32::from(r), u32::from(g), u32::from(b)];
        match out.last_mut() {
            Some(last) if last[..3] == rgb => last[3] += 1,
            _ => out.push([rgb[0], rgb[1], rgb[2], 1]),
        }
    }
    out
}

impl Canvas {
    /// A canvas `height` rows tall, every row `row` (a rendered span row, as a
    /// frame the scanner can read).
    pub fn from_row(row: &[[u8; 3]], height: usize) -> Canvas {
        let mut c = Canvas::new(row.len().max(1), height.max(1), [0, 0, 0]);
        for y in 0..c.height {
            for (x, &rgb) in row.iter().enumerate() {
                c.put(x, y, rgb);
            }
        }
        c
    }
}

/// Catmull-Rom weights for the 4 taps around a fractional offset `t`.
fn cubic_weights(t: f64) -> [f64; 4] {
    const A: f64 = -0.5;
    let k = |x: f64| -> f64 {
        let x = x.abs();
        if x <= 1.0 {
            (A + 2.0) * x * x * x - (A + 3.0) * x * x + 1.0
        } else if x < 2.0 {
            A * x * x * x - 5.0 * A * x * x + 8.0 * A * x - 4.0 * A
        } else {
            0.0
        }
    };
    [k(t + 1.0), k(t), k(1.0 - t), k(2.0 - t)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_coverage_blends_a_half_pixel_edge() {
        let mut c = Canvas::new(4, 1, [0, 0, 0]);
        c.draw_cells(0.5, 0.0, 1.0, 1.0, &[[255, 255, 255]], EdgeMode::Exact);
        assert_eq!(c.get(0, 0), [128, 128, 128]);
        assert_eq!(c.get(1, 0), [128, 128, 128]);
        assert_eq!(c.get(2, 0), [0, 0, 0]);
    }

    #[test]
    fn gamma_correct_midpoint_is_bright() {
        let mut c = Canvas::new(2, 1, [0, 0, 0]);
        c.draw_cells(
            0.5,
            0.0,
            10.0,
            1.0,
            &[[255, 255, 255]],
            EdgeMode::GammaCorrect,
        );
        let v = c.get(0, 0)[0];
        assert!((185..=190).contains(&v), "{v}");
    }

    #[test]
    fn bilinear_upscale_preserves_flat_regions() {
        let mut c = Canvas::new(8, 2, [255, 0, 0]);
        c.fill_rect(4, 0, 4, 2, [0, 0, 255]);
        let up = c.resample(2.0, Filter::Bilinear);
        assert_eq!(up.width(), 16);
        assert_eq!(up.get(1, 1), [255, 0, 0]);
        assert_eq!(up.get(14, 1), [0, 0, 255]);
    }

    #[test]
    fn render_spans_box_filters_fractional_edges_and_blur() {
        // Two 1.5 px spans: pixel 1 straddles the boundary half and half.
        let px = render_spans(
            &[Span::new(1.5, [0, 0, 0]), Span::new(1.5, [255, 255, 255])],
            0.0,
        );
        assert_eq!(px, vec![[0, 0, 0], [128, 128, 128], [255, 255, 255]]);
        // A 1 px blur widens each pixel's window to 3 px, so a crisp edge at
        // x = 4 blends into the pixel either side of it (and the windows clip
        // at the row's ends).
        let px = render_spans(
            &[Span::new(4.0, [0, 0, 0]), Span::new(4.0, [255, 0, 0])],
            1.0,
        );
        let reds: Vec<u8> = px.iter().map(|p| p[0]).collect();
        assert_eq!(reds, vec![0, 0, 0, 85, 170, 255, 255, 255]);
    }

    #[test]
    fn rle_round_trips() {
        let px = vec![[1, 2, 3], [1, 2, 3], [9, 9, 9], [1, 2, 3]];
        let rle = encode_rle(&px);
        assert_eq!(rle, vec![[1, 2, 3, 2], [9, 9, 9, 1], [1, 2, 3, 1]]);
        assert_eq!(decode_rle(&rle), px);
    }

    #[test]
    fn cubic_weights_sum_to_one() {
        for t in [0.0, 0.25, 0.5, 0.9] {
            let s: f64 = cubic_weights(t).iter().sum();
            assert!((s - 1.0).abs() < 1e-9);
        }
    }
}
