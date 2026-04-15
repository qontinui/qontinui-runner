//! Downsample screenshots before embedding them in iteration panels.
//!
//! The iteration canvas panel optionally embeds pre/post thumbnails so
//! users can eyeball *why* the WSM judged a worker action as refused/fail.
//! Full-resolution screenshots (1920×1080+) would blow up the
//! `canvas_panels.data_json` JSONB column on long runs. This helper
//! resamples to 320px wide with Lanczos3, yielding ~15–25KB PNGs.
//!
//! Any failure (malformed base64, decode error, encode error) returns
//! `None` so the caller can skip the embed entirely — we must NEVER fall
//! back to the full-size original, because that defeats the whole point
//! of the bloat guard.

use std::io::Cursor;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use image::{ImageFormat, ImageReader};
use tracing::debug;

/// Default target width for embedded thumbnails. Matches the React side's
/// CSS max-width cap so the thumbnail displays at 1:1 without further
/// browser scaling on typical screens.
pub const THUMBNAIL_TARGET_WIDTH: u32 = 320;

/// Downsample a base64 PNG to at most `target_width` pixels wide, preserving
/// aspect ratio.
///
/// Returns `Some(b64)` on success or when the source is already small
/// enough (in which case the return value is bit-identical to the input).
/// Returns `None` on any failure — callers should skip embedding entirely
/// rather than fall back to the full-size original, because a failing
/// downsample usually means the input is untrusted/malformed data or would
/// be gigantic, both of which defeat the bloat guard this helper exists for.
pub fn downsample_screenshot_for_embed(b64: &str, target_width: u32) -> Option<String> {
    match try_downsample(b64, target_width) {
        Ok(out) => Some(out),
        Err(e) => {
            debug!(
                "screenshot_embed: downsample failed ({}), dropping embed ({} bytes b64 input)",
                e,
                b64.len()
            );
            None
        }
    }
}

fn try_downsample(b64: &str, target_width: u32) -> Result<String, String> {
    if target_width == 0 {
        return Err("target_width must be > 0".to_string());
    }

    let bytes = BASE64_STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;

    let img = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("image format probe: {e}"))?
        .decode()
        .map_err(|e| format!("image decode: {e}"))?;

    let (orig_w, orig_h) = (img.width(), img.height());

    // If the source is already small enough, skip the re-encode to save
    // cycles and avoid any fidelity loss from a pointless round-trip.
    if orig_w <= target_width {
        return Ok(b64.to_string());
    }

    // Preserve aspect ratio. `image::resize` computes the proportional
    // height itself when given a target box — we pass the same width as
    // the box bound and a large height so the width constraint wins.
    let ratio = target_width as f32 / orig_w as f32;
    let target_height = ((orig_h as f32) * ratio).round().max(1.0) as u32;
    let resized = img.resize_exact(
        target_width,
        target_height,
        image::imageops::FilterType::Lanczos3,
    );

    let mut out_bytes: Vec<u8> = Vec::with_capacity(32 * 1024);
    resized
        .write_to(&mut Cursor::new(&mut out_bytes), ImageFormat::Png)
        .map_err(|e| format!("image encode: {e}"))?;

    Ok(BASE64_STANDARD.encode(&out_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn make_rgb_png_b64(w: u32, h: u32) -> String {
        // Build a deterministic RGB gradient so encoding is non-trivial.
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut bytes: Vec<u8> = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode test png");
        BASE64_STANDARD.encode(&bytes)
    }

    fn decode_png_b64(b64: &str) -> (u32, u32) {
        let bytes = BASE64_STANDARD.decode(b64.as_bytes()).expect("decode b64");
        let img = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .expect("probe")
            .decode()
            .expect("decode");
        (img.width(), img.height())
    }

    #[test]
    fn downsamples_1024x768_to_320_width() {
        let src = make_rgb_png_b64(1024, 768);
        let out = downsample_screenshot_for_embed(&src, 320).expect("should downsample");
        let (w, h) = decode_png_b64(&out);
        assert_eq!(w, 320);
        // 768 * (320/1024) = 240
        assert_eq!(h, 240);
    }

    #[test]
    fn preserves_aspect_ratio_on_non_16_9() {
        // Odd aspect ratio — ensure rounding doesn't drop to zero.
        let src = make_rgb_png_b64(1000, 333);
        let out = downsample_screenshot_for_embed(&src, 320).expect("should downsample");
        let (w, h) = decode_png_b64(&out);
        assert_eq!(w, 320);
        // 333 * (320/1000) = 106.56 → rounds to 107
        assert_eq!(h, 107);
    }

    #[test]
    fn passes_through_when_already_small_enough() {
        let src = make_rgb_png_b64(200, 150);
        let out = downsample_screenshot_for_embed(&src, 320).expect("should pass through");
        // Should be byte-identical to input (no re-encode)
        assert_eq!(out, src);
    }

    #[test]
    fn garbage_base64_returns_none() {
        let garbage = "not a valid base64 !!! ???";
        assert_eq!(downsample_screenshot_for_embed(garbage, 320), None);
    }

    #[test]
    fn empty_string_returns_none() {
        // Empty b64 decodes to empty bytes, which is not a valid image.
        assert_eq!(downsample_screenshot_for_embed("", 320), None);
    }

    #[test]
    fn valid_base64_but_not_an_image_returns_none() {
        let not_an_image = BASE64_STANDARD.encode(b"this is just some text, not a png");
        assert_eq!(downsample_screenshot_for_embed(&not_an_image, 320), None);
    }

    #[test]
    fn zero_target_width_returns_none() {
        let src = make_rgb_png_b64(640, 480);
        assert_eq!(downsample_screenshot_for_embed(&src, 0), None);
    }
}
