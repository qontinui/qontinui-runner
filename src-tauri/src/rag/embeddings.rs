//! Embedding generation for RAG
//!
//! This module handles invoking the Python embedding pipeline to generate
//! embeddings for RAG element search.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tracing::{error, info, warn};

/// Errors that can occur during embedding generation
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}

/// Status of embedding generation
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingStatus {
    /// Not started
    NotStarted,

    /// In progress (percentage complete)
    InProgress(u8),

    /// Completed successfully
    Completed,

    /// Failed with error
    Failed(String),
}

/// Progress update from embedding generation
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddingProgress {
    pub status: EmbeddingStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements_processed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_elements: Option<usize>,
}

/// Embedding generator
pub struct EmbeddingGenerator {
    /// Path to Python executable
    python_path: PathBuf,

    /// Path to embedding script
    script_path: PathBuf,

    /// Progress state for each project (project_id -> progress)
    progress_state: Arc<Mutex<HashMap<String, EmbeddingProgress>>>,
}

impl EmbeddingGenerator {
    /// Create a new embedding generator with default paths
    pub fn new() -> Result<Self, EmbeddingError> {
        // Determine Python path
        let python_path = if cfg!(windows) {
            PathBuf::from("python")
        } else {
            PathBuf::from("python3")
        };

        // Find the generate_embeddings.py script in python-bridge directory
        let script_path = Self::find_embeddings_script()?;

        Ok(Self {
            python_path,
            script_path,
            progress_state: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Create a degraded EmbeddingGenerator that will return errors on use
    ///
    /// This is used when RAG initialization fails but we want the runner to continue.
    pub fn new_degraded() -> Self {
        warn!("Creating degraded EmbeddingGenerator - embedding features will be disabled");
        Self {
            python_path: PathBuf::new(),
            script_path: PathBuf::new(),
            progress_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if this is a degraded instance
    pub fn is_degraded(&self) -> bool {
        self.script_path.as_os_str().is_empty()
    }

    /// Find the generate_embeddings.py script in the python-bridge directory
    ///
    /// This searches for the script in multiple locations based on where the
    /// runner is executed from (development vs production).
    fn find_embeddings_script() -> Result<PathBuf, EmbeddingError> {
        const SCRIPT_NAME: &str = "generate_embeddings.py";

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
                EmbeddingError::ConfigError(format!(
                    "Embedding script {} not found in any expected location",
                    SCRIPT_NAME
                ))
            })
    }

    /// Build the async Python command for running the embedding script
    ///
    /// Same as build_python_command but returns tokio::process::Command
    fn build_async_python_command(
        &self,
        project_id: &str,
    ) -> Result<tokio::process::Command, EmbeddingError> {
        let python_bridge_dir = self.script_path.parent().ok_or_else(|| {
            EmbeddingError::ConfigError("Could not determine python-bridge directory".to_string())
        })?;

        // Check for pyproject.toml (poetry project)
        let pyproject_path = python_bridge_dir.join("pyproject.toml");
        let poetry_available = pyproject_path.exists();

        let mut cmd = if poetry_available {
            info!(
                "Using Poetry to run embedding generation from: {:?}",
                python_bridge_dir
            );
            let mut cmd = tokio::process::Command::new("poetry");
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
            let mut cmd = tokio::process::Command::new(&self.python_path);
            cmd.arg("-u"); // Unbuffered output
            cmd.arg(&self.script_path);
            cmd
        };

        // Add script arguments
        cmd.arg("--project-id");
        cmd.arg(project_id);

        Ok(cmd)
    }

    /// Generate embeddings asynchronously (background task)
    ///
    /// This spawns a background task to generate embeddings without blocking.
    /// Progress updates can be monitored via the returned channel and are stored
    /// in the internal progress state.
    ///
    /// # Arguments
    /// * `project_id` - Project ID to generate embeddings for
    ///
    /// # Returns
    /// * Receiver for progress updates
    pub fn generate_embeddings_async(
        &self,
        project_id: String,
    ) -> tokio::sync::mpsc::Receiver<EmbeddingProgress> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        // Build the command before moving into the async block
        let cmd_result = self.build_async_python_command(&project_id);
        let progress_state = self.progress_state.clone();

        tokio::spawn(async move {
            use std::process::Stdio;
            use tokio::io::{AsyncBufReadExt, BufReader};

            // Send initial progress
            let initial_progress = EmbeddingProgress {
                status: EmbeddingStatus::InProgress(0),
                message: "Starting embedding generation...".to_string(),
                percent: Some(0),
                elements_processed: None,
                total_elements: None,
            };
            let _ = tx.send(initial_progress.clone()).await;
            if let Ok(mut state) = progress_state.lock() {
                state.insert(project_id.clone(), initial_progress);
            }

            // Get the command or send error
            let mut cmd = match cmd_result {
                Ok(mut cmd) => {
                    cmd.stdout(Stdio::piped());
                    cmd.stderr(Stdio::piped());
                    cmd
                }
                Err(e) => {
                    let error_progress = EmbeddingProgress {
                        status: EmbeddingStatus::Failed(e.to_string()),
                        message: format!("Failed to build command: {}", e),
                        percent: None,
                        elements_processed: None,
                        total_elements: None,
                    };
                    let _ = tx.send(error_progress.clone()).await;
                    if let Ok(mut state) = progress_state.lock() {
                        state.insert(project_id.clone(), error_progress);
                    }
                    return;
                }
            };

            // Spawn the process
            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    let error_progress = EmbeddingProgress {
                        status: EmbeddingStatus::Failed(e.to_string()),
                        message: format!("Failed to spawn Python process: {}", e),
                        percent: None,
                        elements_processed: None,
                        total_elements: None,
                    };
                    let _ = tx.send(error_progress.clone()).await;
                    if let Ok(mut state) = progress_state.lock() {
                        state.insert(project_id.clone(), error_progress);
                    }
                    return;
                }
            };

            // Read stdout line by line
            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    // Try to parse as JSON
                    if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(&line) {
                        // Parse Python progress JSON
                        let status = json_value.get("status").and_then(|s| s.as_str());
                        let percent = json_value
                            .get("percent")
                            .and_then(|p| p.as_u64())
                            .map(|p| p as u8);
                        let message = json_value
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("");

                        let progress = match status {
                            Some("progress") => EmbeddingProgress {
                                status: EmbeddingStatus::InProgress(percent.unwrap_or(0)),
                                message: message.to_string(),
                                percent,
                                elements_processed: None,
                                total_elements: None,
                            },
                            Some("complete") => {
                                let elements_embedded = json_value
                                    .get("elements_embedded")
                                    .and_then(|e| e.as_u64())
                                    .map(|e| e as usize);
                                let total = json_value
                                    .get("total")
                                    .and_then(|t| t.as_u64())
                                    .map(|t| t as usize);
                                EmbeddingProgress {
                                    status: EmbeddingStatus::Completed,
                                    message: message.to_string(),
                                    percent: Some(100),
                                    elements_processed: elements_embedded,
                                    total_elements: total,
                                }
                            }
                            Some("error") => EmbeddingProgress {
                                status: EmbeddingStatus::Failed(message.to_string()),
                                message: message.to_string(),
                                percent: None,
                                elements_processed: None,
                                total_elements: None,
                            },
                            _ => continue,
                        };

                        // Update progress state
                        if let Ok(mut state) = progress_state.lock() {
                            state.insert(project_id.clone(), progress.clone());
                        }

                        // Send to channel
                        let _ = tx.send(progress).await;
                    } else {
                        // Non-JSON output, log it
                        info!("Python output: {}", line);
                    }
                }
            }

            // Wait for process to complete
            match child.wait().await {
                Ok(status) => {
                    if !status.success() {
                        // Read stderr
                        let stderr = if let Some(mut stderr) = child.stderr {
                            let mut buf = Vec::new();
                            use tokio::io::AsyncReadExt;
                            let _ = stderr.read_to_end(&mut buf).await;
                            String::from_utf8_lossy(&buf).to_string()
                        } else {
                            String::new()
                        };

                        let error_progress = EmbeddingProgress {
                            status: EmbeddingStatus::Failed(stderr.clone()),
                            message: format!("Python process exited with error: {}", stderr),
                            percent: None,
                            elements_processed: None,
                            total_elements: None,
                        };
                        let _ = tx.send(error_progress.clone()).await;
                        if let Ok(mut state) = progress_state.lock() {
                            state.insert(project_id.clone(), error_progress);
                        }
                    }
                }
                Err(e) => {
                    let error_progress = EmbeddingProgress {
                        status: EmbeddingStatus::Failed(e.to_string()),
                        message: format!("Failed to wait for Python process: {}", e),
                        percent: None,
                        elements_processed: None,
                        total_elements: None,
                    };
                    let _ = tx.send(error_progress.clone()).await;
                    if let Ok(mut state) = progress_state.lock() {
                        state.insert(project_id.clone(), error_progress);
                    }
                }
            }
        });

        rx
    }

    /// Get current progress for a project
    ///
    /// # Arguments
    /// * `project_id` - Project ID
    ///
    /// # Returns
    /// * Current progress or None if not tracked
    pub fn get_progress(&self, project_id: &str) -> Option<EmbeddingProgress> {
        if let Ok(state) = self.progress_state.lock() {
            state.get(project_id).cloned()
        } else {
            None
        }
    }

    /// Check the status of embedding generation for a project
    ///
    /// This checks if embeddings exist for the given project.
    ///
    /// # Arguments
    /// * `project_id` - Project ID to check
    ///
    /// # Returns
    /// * `EmbeddingStatus` - Current status
    pub fn check_status(&self, project_id: &str) -> EmbeddingStatus {
        // First check if we have progress state (in-progress)
        if let Some(progress) = self.get_progress(project_id) {
            return progress.status;
        }

        // Check if embeddings directory exists
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                return EmbeddingStatus::Failed("Could not determine home directory".to_string())
            }
        };

        let embeddings_path = home
            .join(".qontinui")
            .join("rag")
            .join(project_id)
            .join("embeddings");

        if embeddings_path.exists() && embeddings_path.join("embeddings.json").exists() {
            EmbeddingStatus::Completed
        } else {
            EmbeddingStatus::NotStarted
        }
    }
}

impl Default for EmbeddingGenerator {
    fn default() -> Self {
        Self::new().expect("Failed to create default EmbeddingGenerator")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_generator_creation() {
        let generator = EmbeddingGenerator::new();
        assert!(generator.is_ok());
    }

    #[test]
    fn test_check_status_not_started() {
        let generator = EmbeddingGenerator::new().unwrap();
        let status = generator.check_status("nonexistent-project");
        matches!(status, EmbeddingStatus::NotStarted);
    }
}
