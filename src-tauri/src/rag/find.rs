//! RAG Find - SAM3 segmentation and vector matching
//!
//! This module handles invoking the Python find_rag.py script to:
//! 1. Segment screenshots using SAM3
//! 2. Match segments against indexed StateImages in the vector database
//! 3. Return segments with their matches for visual automation

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during RAG find operation
#[derive(Debug, Error)]
pub enum FindError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// Bounding box from find result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindBoundingBox {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Center point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Match information from the vector database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMatch {
    pub element_id: String,
    pub element_name: String,
    pub visual_similarity: f32,
    pub text_similarity: Option<f32>,
    pub combined_score: f32,
    pub element_type: Option<String>,
    pub text_description: String,
    pub state_id: String,
}

/// Segment information from SAM3 segmentation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindSegment {
    pub index: usize,
    pub bbox: FindBoundingBox,
    pub center: Point,
    pub area: i32,
    pub confidence: f32,
    pub text_description: String,
    pub ocr_text: Option<String>,
    pub mask_base64: String,
    pub image_base64: String,
    pub matches: Vec<SegmentMatch>,
}

/// Screenshot size
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotSize {
    pub width: i32,
    pub height: i32,
}

/// Find response from Python script
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResponse {
    pub success: bool,
    pub project_id: String,
    pub screenshot_size: ScreenshotSize,
    pub total_segments: usize,
    pub total_elements: usize,
    pub min_similarity: f32,
    pub segments: Vec<FindSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// RAG Element Finder
pub struct RAGFinder {
    // Empty struct - fields removed as they were unused
}

impl RAGFinder {
    /// Create a new RAG finder with default paths
    pub fn new() -> Result<Self, FindError> {
        Ok(Self {})
    }
}

impl Default for RAGFinder {
    fn default() -> Self {
        Self::new().expect("Failed to create default RAGFinder")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rag_finder_creation() {
        let finder = RAGFinder::new();
        assert!(finder.is_ok());
    }
}
