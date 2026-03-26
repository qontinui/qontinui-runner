//! Screenshot Annotator
//!
//! Draws numbered colored rectangles on screenshots to identify interactive
//! UI elements for LLM consumption. Produces both an annotated image (base64)
//! and a text index that can be included in prompts.
//!
//! Inspired by TuriX-CUA's Set-of-Mark annotation pattern: the LLM references
//! elements by number ("click element #3") rather than guessing coordinates.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::draw_hollow_rect_mut;
use imageproc::rect::Rect;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use tracing::debug;

pub use super::types::NormalizedRect;

// =============================================================================
// Types
// =============================================================================

/// A UI element to annotate on a screenshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedElement {
    /// Sequential index (1-based) for LLM reference.
    pub index: u32,
    /// Human-readable label (e.g., "Login", "Submit").
    pub label: String,
    /// Element type (e.g., "button", "input", "link", "text").
    pub element_type: String,
    /// Bounding box in normalized 0.0-1.0 coordinates.
    pub normalized_rect: NormalizedRect,
}

/// Result of annotating a screenshot.
#[derive(Debug, Clone)]
pub struct AnnotationResult {
    /// Base64-encoded annotated PNG image.
    pub annotated_base64: String,
    /// Text index listing all elements with their positions.
    /// Format: `[1] "Login" button at (0.12, 0.34) size (0.08, 0.04)`
    pub element_index_text: String,
    /// Number of elements annotated.
    pub element_count: usize,
}

// =============================================================================
// Color Scheme
// =============================================================================

/// Color for annotation boxes based on element type.
fn color_for_type(element_type: &str) -> Rgba<u8> {
    match element_type.to_lowercase().as_str() {
        "button" | "submit" => Rgba([0, 200, 0, 255]),     // Green
        "input" | "textarea" => Rgba([0, 100, 255, 255]),  // Blue
        "link" | "a" => Rgba([255, 140, 0, 255]),          // Orange
        "select" | "dropdown" => Rgba([180, 0, 180, 255]), // Purple
        "checkbox" | "radio" => Rgba([0, 180, 180, 255]),  // Teal
        _ => Rgba([180, 180, 180, 255]),                   // Gray
    }
}

// =============================================================================
// Pixel-Based Number Drawing (no font dependency)
// =============================================================================

/// 5x7 pixel patterns for digits 0-9.
const DIGIT_PATTERNS: [&[u8; 35]; 10] = [
    // 0
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 0, 0, 1, 1, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
    // 1
    &[
        0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1,
        0, 0, 1, 1, 1, 1, 1,
    ],
    // 2
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 0, 0,
        0, 0, 1, 1, 1, 1, 1,
    ],
    // 3
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 1, 1, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
    // 4
    &[
        0, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 0, 1, 0, 0, 1, 0, 1, 1, 1, 1, 1, 0, 0, 0,
        1, 0, 0, 0, 0, 1, 0,
    ],
    // 5
    &[
        1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
    // 6
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
    // 7
    &[
        1, 1, 1, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        0, 0, 0, 1, 0, 0, 0,
    ],
    // 8
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
    // 9
    &[
        0, 1, 1, 1, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0,
        0, 1, 0, 1, 1, 1, 0,
    ],
];

/// Draw a number at the given pixel position using 5x7 pixel font.
/// Each digit is 5px wide + 1px gap = 6px per digit.
fn draw_number(img: &mut RgbaImage, number: u32, x: i32, y: i32, color: Rgba<u8>, scale: u32) {
    let digits: Vec<u32> = if number == 0 {
        vec![0]
    } else {
        let mut n = number;
        let mut d = Vec::new();
        while n > 0 {
            d.push(n % 10);
            n /= 10;
        }
        d.reverse();
        d
    };

    let (img_w, img_h) = (img.width(), img.height());
    let mut dx_offset = 0_i32;

    for digit in digits {
        let pattern = DIGIT_PATTERNS[digit as usize];
        for row in 0..7_i32 {
            for col in 0..5_i32 {
                if pattern[(row * 5 + col) as usize] == 1 {
                    // Draw scaled pixel
                    for sy in 0..scale as i32 {
                        for sx in 0..scale as i32 {
                            let px = x + dx_offset + col * scale as i32 + sx;
                            let py = y + row * scale as i32 + sy;
                            if px >= 0 && py >= 0 && (px as u32) < img_w && (py as u32) < img_h {
                                img.put_pixel(px as u32, py as u32, color);
                            }
                        }
                    }
                }
            }
        }
        dx_offset += 6 * scale as i32; // 5px digit + 1px gap, scaled
    }
}

/// Calculate the width in pixels of a number label at the given scale.
fn number_label_width(number: u32, scale: u32) -> i32 {
    // Use string length for robust digit counting (avoids f64 log10 edge cases)
    let digit_count = number.to_string().len() as i32;
    digit_count * 6 * scale as i32 - scale as i32 // subtract trailing gap
}

// =============================================================================
// Annotation Engine
// =============================================================================

/// Annotate a screenshot with numbered element boxes.
///
/// Takes a base64-encoded screenshot and a list of elements, draws numbered
/// colored rectangles on the image, and returns both the annotated image and
/// a text index.
///
/// # Arguments
/// * `screenshot_base64` - Base64-encoded PNG image (no data URL prefix).
/// * `elements` - Elements to annotate, with normalized coordinates.
/// * `max_width` - Maximum image width after resize. Set to 0 to skip resize.
///   Default recommendation: 1024 (saves ~75% tokens vs 1920px).
///
/// # Returns
/// `AnnotationResult` with annotated image base64 and text index.
pub fn annotate_screenshot(
    screenshot_base64: &str,
    elements: &[AnnotatedElement],
    max_width: u32,
) -> Result<AnnotationResult, String> {
    // Decode base64 to image
    let image_bytes = BASE64
        .decode(screenshot_base64)
        .map_err(|e| format!("Failed to decode screenshot base64: {}", e))?;

    let mut img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("Failed to load screenshot image: {}", e))?;

    // Resize if wider than max_width
    let (orig_w, orig_h) = (img.width(), img.height());
    if max_width > 0 && orig_w > max_width {
        let new_height = (orig_h as f64 * max_width as f64 / orig_w as f64) as u32;
        img = img.resize(max_width, new_height, image::imageops::FilterType::Lanczos3);
        debug!(
            "Resized screenshot from {}x{} to {}x{}",
            orig_w,
            orig_h,
            img.width(),
            img.height()
        );
    }

    let (img_w, img_h) = (img.width(), img.height());
    let mut rgba = img.to_rgba8();

    // Build text index
    let mut index_lines = Vec::with_capacity(elements.len());

    // Scale for number labels: 2px for images >500px, 1px for smaller
    let label_scale = if img_w > 500 { 2_u32 } else { 1 };

    for elem in elements {
        // Convert normalized coords to pixel coords
        let px_x = (elem.normalized_rect.x * img_w as f32) as i32;
        let px_y = (elem.normalized_rect.y * img_h as f32) as i32;
        let px_w = (elem.normalized_rect.width * img_w as f32) as i32;
        let px_h = (elem.normalized_rect.height * img_h as f32) as i32;

        // Clamp to image bounds
        let px_x = px_x.max(0).min(img_w as i32 - 1);
        let px_y = px_y.max(0).min(img_h as i32 - 1);
        let px_w = px_w.max(1).min(img_w as i32 - px_x);
        let px_h = px_h.max(1).min(img_h as i32 - px_y);

        let color = color_for_type(&elem.element_type);

        // Draw rectangle outline (2px thick by drawing twice)
        let rect = Rect::at(px_x, px_y).of_size(px_w as u32, px_h as u32);
        draw_hollow_rect_mut(&mut rgba, rect, color);
        if px_w > 2 && px_h > 2 {
            let inner = Rect::at(px_x + 1, px_y + 1).of_size(
                (px_w - 2).max(1) as u32,
                (px_h - 2).max(1) as u32,
            );
            draw_hollow_rect_mut(&mut rgba, inner, color);
        }

        // Draw number label background (colored rectangle at top-left)
        let label_w = number_label_width(elem.index, label_scale) + 4;
        let label_h = 7 * label_scale as i32 + 4;
        let label_w = label_w.min(px_w);
        let label_h = label_h.min(px_h);

        for dy in 0..label_h {
            for dx in 0..label_w {
                let x = (px_x + dx) as u32;
                let y = (px_y + dy) as u32;
                if x < img_w && y < img_h {
                    rgba.put_pixel(x, y, color);
                }
            }
        }

        // Draw number in white on the colored background
        draw_number(
            &mut rgba,
            elem.index,
            px_x + 2,
            px_y + 2,
            Rgba([255, 255, 255, 255]),
            label_scale,
        );

        // Build text index line
        index_lines.push(format!(
            "[{}] \"{}\" {} at ({:.3}, {:.3}) size ({:.3}, {:.3})",
            elem.index,
            elem.label,
            elem.element_type,
            elem.normalized_rect.x,
            elem.normalized_rect.y,
            elem.normalized_rect.width,
            elem.normalized_rect.height,
        ));
    }

    // Encode annotated image back to base64 PNG
    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png_bytes));
    DynamicImage::ImageRgba8(rgba)
        .write_with_encoder(encoder)
        .map_err(|e| format!("Failed to encode annotated screenshot: {}", e))?;

    let annotated_base64 = BASE64.encode(&png_bytes);

    let element_index_text = if index_lines.is_empty() {
        "No interactive elements detected.".to_string()
    } else {
        format!("## Screen Elements\n{}", index_lines.join("\n"))
    };

    debug!(
        "Annotated screenshot with {} elements ({}x{} -> {} bytes base64)",
        elements.len(),
        img_w,
        img_h,
        annotated_base64.len()
    );

    Ok(AnnotationResult {
        annotated_base64,
        element_index_text,
        element_count: elements.len(),
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a small white PNG image encoded as base64.
    fn make_white_png(width: u32, height: u32) -> String {
        let img = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));
        let mut png_bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png_bytes));
        DynamicImage::ImageRgba8(img)
            .write_with_encoder(encoder)
            .unwrap();
        BASE64.encode(&png_bytes)
    }

    #[test]
    fn test_annotate_empty_elements() {
        let base64 = make_white_png(100, 100);
        let result = annotate_screenshot(&base64, &[], 0).unwrap();
        assert_eq!(result.element_count, 0);
        assert!(result.element_index_text.contains("No interactive elements"));
        assert!(!result.annotated_base64.is_empty());
    }

    #[test]
    fn test_annotate_with_elements() {
        let base64 = make_white_png(200, 200);
        let elements = vec![
            AnnotatedElement {
                index: 1,
                label: "Login".to_string(),
                element_type: "button".to_string(),
                normalized_rect: NormalizedRect {
                    x: 0.1,
                    y: 0.1,
                    width: 0.3,
                    height: 0.1,
                },
            },
            AnnotatedElement {
                index: 2,
                label: "Email".to_string(),
                element_type: "input".to_string(),
                normalized_rect: NormalizedRect {
                    x: 0.1,
                    y: 0.3,
                    width: 0.5,
                    height: 0.08,
                },
            },
            AnnotatedElement {
                index: 3,
                label: "Sign Up".to_string(),
                element_type: "link".to_string(),
                normalized_rect: NormalizedRect {
                    x: 0.1,
                    y: 0.5,
                    width: 0.2,
                    height: 0.05,
                },
            },
        ];

        let result = annotate_screenshot(&base64, &elements, 0).unwrap();
        assert_eq!(result.element_count, 3);
        assert!(result.element_index_text.contains("[1] \"Login\" button"));
        assert!(result.element_index_text.contains("[2] \"Email\" input"));
        assert!(result.element_index_text.contains("[3] \"Sign Up\" link"));

        // Verify the annotated image is valid base64 that decodes to a PNG
        let decoded = BASE64.decode(&result.annotated_base64).unwrap();
        let img = image::load_from_memory(&decoded).unwrap();
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 200);
    }

    #[test]
    fn test_annotate_with_resize() {
        let base64 = make_white_png(1920, 1080);
        let elements = vec![AnnotatedElement {
            index: 1,
            label: "Button".to_string(),
            element_type: "button".to_string(),
            normalized_rect: NormalizedRect {
                x: 0.5,
                y: 0.5,
                width: 0.1,
                height: 0.05,
            },
        }];

        let result = annotate_screenshot(&base64, &elements, 1024).unwrap();
        assert_eq!(result.element_count, 1);

        // Verify image was resized
        let decoded = BASE64.decode(&result.annotated_base64).unwrap();
        let img = image::load_from_memory(&decoded).unwrap();
        assert_eq!(img.width(), 1024);
        // Height should be proportionally scaled: 1080 * 1024/1920 ≈ 576
        assert!(img.height() > 500 && img.height() < 600);
    }

    #[test]
    fn test_color_for_type() {
        assert_eq!(color_for_type("button"), Rgba([0, 200, 0, 255]));
        assert_eq!(color_for_type("input"), Rgba([0, 100, 255, 255]));
        assert_eq!(color_for_type("link"), Rgba([255, 140, 0, 255]));
        assert_eq!(color_for_type("unknown"), Rgba([180, 180, 180, 255]));
    }

    #[test]
    fn test_number_label_width() {
        assert_eq!(number_label_width(1, 1), 5); // 1 digit: 5px
        assert_eq!(number_label_width(12, 1), 11); // 2 digits: 5+1+5 = 11px
        assert_eq!(number_label_width(1, 2), 10); // 1 digit at 2x: 10px
    }

    #[test]
    fn test_invalid_base64() {
        let result = annotate_screenshot("not-valid-base64!!!", &[], 0);
        assert!(result.is_err());
    }
}
