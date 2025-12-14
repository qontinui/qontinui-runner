//! RAG Find - SAM3 segmentation and vector matching
//!
//! This module handles invoking the Python find_rag.py script to:
//! 1. Segment screenshots using SAM3
//! 2. Match segments against indexed StateImages in the vector database
//! 3. Return segments with their matches for visual automation

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;
use tracing::{error, info};

/// Errors that can occur during RAG find operation
#[derive(Debug, Error)]
pub enum FindError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Python execution error: {0}")]
    PythonError(String),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Embeddings not found: {0}")]
    EmbeddingsNotFound(String),
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
    /// Path to Python executable
    python_path: PathBuf,

    /// Path to find script
    script_path: PathBuf,
}

impl RAGFinder {
    /// Create a new RAG finder with default paths
    pub fn new() -> Result<Self, FindError> {
        // Determine Python path
        let python_path = if cfg!(windows) {
            PathBuf::from("python")
        } else {
            PathBuf::from("python3")
        };

        // Find the find_rag.py script in python-bridge directory
        let script_path = Self::find_script()?;

        Ok(Self {
            python_path,
            script_path,
        })
    }

    /// Find the find_rag.py script in the python-bridge directory
    fn find_script() -> Result<PathBuf, FindError> {
        const SCRIPT_NAME: &str = "find_rag.py";

        let possible_paths = vec![
            // When running from src-tauri/target/debug or release
            std::env::current_dir().ok().and_then(|p| {
                if p.ends_with("debug") || p.ends_with("release") {
                    p.parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                        .map(|p| p.join("python-bridge").join(SCRIPT_NAME))
                } else if p.ends_with("src-tauri") {
                    p.parent()
                        .map(|p| p.join("python-bridge").join(SCRIPT_NAME))
                } else {
                    None
                }
            }),
            // When running from qontinui-runner directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("python-bridge").join(SCRIPT_NAME)),
            // When in src-tauri directory
            std::env::current_dir()
                .ok()
                .map(|p| p.join("..").join("python-bridge").join(SCRIPT_NAME)),
        ];

        possible_paths
            .into_iter()
            .flatten()
            .find(|p| p.exists())
            .ok_or_else(|| {
                FindError::ConfigError(format!(
                    "Find script {} not found in any expected location",
                    SCRIPT_NAME
                ))
            })
    }

    /// Build the Python command for running the find script
    fn build_python_command(
        &self,
        project_id: &str,
        screenshot_path: Option<&str>,
        screenshot_base64: Option<&str>,
        capture_screen: bool,
        monitor: Option<i32>,
        min_similarity: Option<f32>,
        max_segments: Option<usize>,
        enable_ocr: bool,
    ) -> Result<Command, FindError> {
        let python_bridge_dir = self.script_path.parent().ok_or_else(|| {
            FindError::ConfigError("Could not determine python-bridge directory".to_string())
        })?;

        // Check for pyproject.toml (poetry project)
        let pyproject_path = python_bridge_dir.join("pyproject.toml");
        let poetry_available = pyproject_path.exists();

        let mut cmd = if poetry_available {
            info!("Using Poetry to run RAG find from: {:?}", python_bridge_dir);
            let mut cmd = Command::new("poetry");
            cmd.current_dir(python_bridge_dir);
            cmd.arg("run");
            cmd.arg("python");
            cmd.arg("-u"); // Unbuffered output
            cmd.arg(&self.script_path);
            cmd
        } else {
            info!(
                "Poetry not found, using system Python: {:?}",
                self.python_path
            );
            let mut cmd = Command::new(&self.python_path);
            cmd.arg("-u"); // Unbuffered output
            cmd.arg(&self.script_path);
            cmd
        };

        // Add script arguments
        cmd.arg("--project-id");
        cmd.arg(project_id);

        // Screenshot source (mutually exclusive)
        if let Some(path) = screenshot_path {
            cmd.arg("--screenshot");
            cmd.arg(path);
        } else if let Some(b64) = screenshot_base64 {
            cmd.arg("--screenshot-base64");
            cmd.arg(b64);
        } else if capture_screen {
            cmd.arg("--capture-screen");
            if let Some(mon) = monitor {
                cmd.arg("--monitor");
                cmd.arg(mon.to_string());
            }
        }

        // Optional parameters
        if let Some(sim) = min_similarity {
            cmd.arg("--min-similarity");
            cmd.arg(sim.to_string());
        }

        if let Some(max) = max_segments {
            cmd.arg("--max-segments");
            cmd.arg(max.to_string());
        }

        if enable_ocr {
            cmd.arg("--enable-ocr");
        }

        Ok(cmd)
    }

    /// Find matching elements in a screenshot
    ///
    /// This spawns a Python subprocess to:
    /// 1. Segment the screenshot using SAM3
    /// 2. Vectorize each segment with CLIP
    /// 3. Match against all indexed elements in the vector database
    ///
    /// # Arguments
    /// * `project_id` - Project ID to search in
    /// * `screenshot_path` - Optional path to screenshot file
    /// * `screenshot_base64` - Optional base64 encoded screenshot
    /// * `capture_screen` - Whether to capture the screen
    /// * `monitor` - Monitor index for screen capture
    /// * `min_similarity` - Minimum similarity threshold (0.0-1.0)
    /// * `max_segments` - Maximum segments to process
    /// * `enable_ocr` - Whether to enable OCR text extraction
    ///
    /// # Returns
    /// * `Ok(FindResponse)` - Segments with their matches
    /// * `Err(FindError)` - Error occurred during find operation
    #[allow(clippy::too_many_arguments)]
    pub fn find(
        &self,
        project_id: &str,
        screenshot_path: Option<&str>,
        screenshot_base64: Option<&str>,
        capture_screen: bool,
        monitor: Option<i32>,
        min_similarity: Option<f32>,
        max_segments: Option<usize>,
        enable_ocr: bool,
    ) -> Result<FindResponse, FindError> {
        info!(
            "RAG find: project_id={}, capture_screen={}, min_similarity={:?}",
            project_id, capture_screen, min_similarity
        );

        let mut cmd = self.build_python_command(
            project_id,
            screenshot_path,
            screenshot_base64,
            capture_screen,
            monitor,
            min_similarity,
            max_segments,
            enable_ocr,
        )?;

        info!("Executing RAG find command: {:?}", cmd);

        let output = cmd.output()?;

        // Parse JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("RAG find failed: {}", stderr);

            // Try to parse error from stdout JSON
            if let Ok(response) = serde_json::from_str::<FindResponse>(&stdout) {
                if let Some(err) = response.error {
                    if err.contains("not found") {
                        return Err(FindError::EmbeddingsNotFound(err));
                    }
                    return Err(FindError::PythonError(err));
                }
            }

            return Err(FindError::PythonError(stderr.to_string()));
        }

        // Parse successful response
        let response: FindResponse = serde_json::from_str(&stdout)?;

        if !response.success {
            if let Some(err) = response.error {
                if err.contains("not found") {
                    return Err(FindError::EmbeddingsNotFound(err));
                }
                return Err(FindError::PythonError(err));
            }
        }

        info!(
            "RAG find completed: found {} segments with matches against {} elements",
            response.segments.len(),
            response.total_elements
        );

        Ok(response)
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
