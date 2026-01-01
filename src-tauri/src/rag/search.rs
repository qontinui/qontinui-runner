//! Semantic search for RAG elements
//!
//! This module handles invoking the Python semantic search script to query
//! the Qdrant vector database and retrieve similar GUI elements.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;
use tracing::{error, info, warn};

/// Errors that can occur during search
#[derive(Debug, Error)]
pub enum SearchError {
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

/// Semantic search result from Python script
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchResult {
    pub element_id: String,
    pub name: String,
    pub score: f32,
    pub element_type: Option<String>,
    pub text_description: String,
    pub source_screenshot_id: String,
    pub state_id: String,
    pub bounding_box: Option<BoundingBox>,
    pub metadata: serde_json::Value,
}

/// Bounding box from search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Search response from Python script
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchResponse {
    success: bool,
    query: String,
    result_count: usize,
    results: Vec<SemanticSearchResult>,
    error: Option<String>,
}

/// Semantic search executor
pub struct SemanticSearch {
    /// Path to Python executable
    python_path: PathBuf,

    /// Path to search script
    script_path: PathBuf,
}

impl SemanticSearch {
    /// Create a new semantic search executor with default paths
    pub fn new() -> Result<Self, SearchError> {
        // Determine Python path
        let python_path = if cfg!(windows) {
            PathBuf::from("python")
        } else {
            PathBuf::from("python3")
        };

        // Find the search_rag.py script in python-bridge directory
        let script_path = Self::find_search_script()?;

        Ok(Self {
            python_path,
            script_path,
        })
    }

    /// Create a degraded SemanticSearch that will return errors on use
    ///
    /// This is used when RAG initialization fails but we want the runner to continue.
    pub fn new_degraded() -> Self {
        warn!("Creating degraded SemanticSearch - semantic search features will be disabled");
        Self {
            python_path: PathBuf::new(),
            script_path: PathBuf::new(),
        }
    }

    /// Check if this is a degraded instance
    #[allow(dead_code)]
    pub fn is_degraded(&self) -> bool {
        self.script_path.as_os_str().is_empty()
    }

    /// Find the search_rag.py script in the python-bridge directory
    fn find_search_script() -> Result<PathBuf, SearchError> {
        const SCRIPT_NAME: &str = "search_rag.py";

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
                SearchError::ConfigError(format!(
                    "Search script {} not found in any expected location",
                    SCRIPT_NAME
                ))
            })
    }

    /// Build the Python command for running the search script
    fn build_python_command(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
        min_score: Option<f32>,
    ) -> Result<Command, SearchError> {
        let python_bridge_dir = self.script_path.parent().ok_or_else(|| {
            SearchError::ConfigError("Could not determine python-bridge directory".to_string())
        })?;

        // Check for pyproject.toml (poetry project)
        let pyproject_path = python_bridge_dir.join("pyproject.toml");
        let poetry_available = pyproject_path.exists();

        let mut cmd = if poetry_available {
            info!(
                "Using Poetry to run semantic search from: {:?}",
                python_bridge_dir
            );
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
        cmd.arg("--query");
        cmd.arg(query);
        cmd.arg("--limit");
        cmd.arg(limit.to_string());

        if let Some(score) = min_score {
            cmd.arg("--min-score");
            cmd.arg(score.to_string());
        }

        Ok(cmd)
    }

    /// Search for GUI elements using semantic similarity
    ///
    /// This spawns a Python subprocess to perform vector search against the Qdrant database.
    ///
    /// # Arguments
    /// * `project_id` - Project ID to search in
    /// * `query` - Search query text
    /// * `limit` - Maximum number of results
    /// * `min_score` - Optional minimum similarity score (0.0-1.0)
    ///
    /// # Returns
    /// * `Ok(Vec<SemanticSearchResult>)` - Search results with similarity scores
    /// * `Err(SearchError)` - Error occurred during search
    pub fn search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
        min_score: Option<f32>,
    ) -> Result<Vec<SemanticSearchResult>, SearchError> {
        info!(
            "Semantic search: project_id={}, query='{}', limit={}",
            project_id, query, limit
        );

        let mut cmd = self.build_python_command(project_id, query, limit, min_score)?;

        info!("Executing semantic search command: {:?}", cmd);

        let output = cmd.output()?;

        // Parse JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Semantic search failed: {}", stderr);

            // Try to parse error from stdout JSON
            if let Ok(response) = serde_json::from_str::<SearchResponse>(&stdout) {
                if let Some(err) = response.error {
                    if err.contains("not found") {
                        return Err(SearchError::EmbeddingsNotFound(err));
                    }
                    return Err(SearchError::PythonError(err));
                }
            }

            return Err(SearchError::PythonError(stderr.to_string()));
        }

        // Parse successful response
        let response: SearchResponse = serde_json::from_str(&stdout)?;

        if !response.success {
            if let Some(err) = response.error {
                if err.contains("not found") {
                    return Err(SearchError::EmbeddingsNotFound(err));
                }
                return Err(SearchError::PythonError(err));
            }
        }

        info!(
            "Semantic search completed: found {} results",
            response.results.len()
        );

        Ok(response.results)
    }
}

impl Default for SemanticSearch {
    fn default() -> Self {
        Self::new().expect("Failed to create default SemanticSearch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_search_creation() {
        let search = SemanticSearch::new();
        assert!(search.is_ok());
    }
}
