//! Shared vision types used across the annotation engine and step outputs.

use serde::{Deserialize, Serialize};

/// Bounding box in normalized 0.0-1.0 coordinate space (screen-relative).
///
/// Resolution-independent representation used for portable element references,
/// LLM-friendly coordinate output, and screenshot annotation.
///
/// This is the canonical type — used by both `DetectedElement` in step outputs
/// and `AnnotatedElement` in the annotation engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRect {
    /// Center point of the normalized rect.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}
