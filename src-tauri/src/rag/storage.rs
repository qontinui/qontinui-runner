//! RAG storage management
//!
//! Handles saving and loading RAG configurations and images to/from disk.
//!
//! # Storage Structure
//!
//! ```text
//! ~/.qontinui/rag/
//!   ├── {project_id}/
//!   │   ├── config.json              # Full QontinuiConfig (source of truth)
//!   │   ├── images/                  # Extracted images from config.images[]
//!   │   │   ├── {image_id}.png
//!   │   │   └── ...
//!   │   └── embeddings/              # Generated embeddings (by Python)
//!   │       ├── embeddings.json
//!   │       └── index.bin
//!   └── ...
//! ```

use super::{ImageAsset, QontinuiConfig, RAGConfigSummary};
use base64::{engine::general_purpose, Engine};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{error, info, warn};

/// Errors that can occur during RAG storage operations
#[derive(Debug, Error)]
pub enum RAGStorageError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Base64 decode error: {0}")]
    Base64Error(#[from] base64::DecodeError),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),
}

/// RAG storage manager
pub struct RAGStorage {
    /// Base directory for RAG storage (~/.qontinui/rag/)
    base_path: PathBuf,
}

impl RAGStorage {
    /// Create a new RAGStorage with default path
    pub fn new() -> Result<Self, RAGStorageError> {
        let home = dirs::home_dir().ok_or_else(|| {
            RAGStorageError::InvalidPath("Could not determine home directory".to_string())
        })?;

        let base_path = home.join(".qontinui").join("rag");

        // Create base directory if it doesn't exist
        fs::create_dir_all(&base_path)?;

        info!("RAG storage initialized at: {:?}", base_path);

        Ok(Self { base_path })
    }

    /// Create a new RAGStorage with custom base path
    #[allow(dead_code)]
    pub fn with_path(base_path: PathBuf) -> Result<Self, RAGStorageError> {
        fs::create_dir_all(&base_path)?;
        info!("RAG storage initialized at: {:?}", base_path);
        Ok(Self { base_path })
    }

    /// Get the path for a specific project
    pub fn get_project_path(&self, project_id: &str) -> PathBuf {
        self.base_path.join(project_id)
    }

    /// Get the path for the config file
    pub fn get_config_path(&self, project_id: &str) -> PathBuf {
        self.get_project_path(project_id).join("config.json")
    }

    /// Get the path for the images directory (extracted from config.images[])
    pub fn get_images_path(&self, project_id: &str) -> PathBuf {
        self.get_project_path(project_id).join("images")
    }

    /// Get the path for the embeddings directory
    pub fn get_embeddings_path(&self, project_id: &str) -> PathBuf {
        self.get_project_path(project_id).join("embeddings")
    }

    /// Check if a config exists for a project
    pub fn config_exists(&self, project_id: &str) -> bool {
        self.get_config_path(project_id).exists()
    }

    /// Save a QontinuiConfig to disk
    pub fn save_qontinui_config(
        &self,
        project_id: &str,
        config: &QontinuiConfig,
    ) -> Result<PathBuf, RAGStorageError> {
        let project_path = self.get_project_path(project_id);
        fs::create_dir_all(&project_path)?;

        let config_path = self.get_config_path(project_id);
        let config_json = serde_json::to_string_pretty(config)?;

        let mut file = fs::File::create(&config_path)?;
        file.write_all(config_json.as_bytes())?;

        info!(
            "QontinuiConfig saved: project_id={}, path={:?}, states={}, patterns={}",
            project_id,
            config_path,
            config.states.len(),
            config.pattern_count()
        );

        Ok(config_path)
    }

    /// Load a QontinuiConfig from disk
    pub fn load_qontinui_config(
        &self,
        project_id: &str,
    ) -> Result<QontinuiConfig, RAGStorageError> {
        let config_path = self.get_config_path(project_id);

        if !config_path.exists() {
            return Err(RAGStorageError::ProjectNotFound(project_id.to_string()));
        }

        let config_json = fs::read_to_string(&config_path)?;
        let config: QontinuiConfig = serde_json::from_str(&config_json)?;

        info!(
            "QontinuiConfig loaded: project_id={}, states={}, patterns={}",
            project_id,
            config.states.len(),
            config.pattern_count()
        );

        Ok(config)
    }

    /// Save images from config to disk (only saves referenced images)
    pub fn save_images_from_config(
        &self,
        project_id: &str,
        images: &[ImageAsset],
        referenced_ids: &[String],
    ) -> Result<usize, RAGStorageError> {
        let images_path = self.get_images_path(project_id);
        fs::create_dir_all(&images_path)?;

        let mut saved_count = 0;

        for image in images {
            // Only save images that are referenced by patterns
            if !referenced_ids.contains(&image.id) {
                continue;
            }

            // Determine file extension from format
            let ext = match image.format.to_lowercase().as_str() {
                "png" => "png",
                "jpg" | "jpeg" => "jpg",
                "webp" => "webp",
                "gif" => "gif",
                _ => "png", // Default to png
            };

            let image_path = images_path.join(format!("{}.{}", image.id, ext));

            // Decode base64 image data
            let image_bytes = general_purpose::STANDARD.decode(&image.data)?;

            // Write to file
            let mut file = fs::File::create(&image_path)?;
            file.write_all(&image_bytes)?;

            saved_count += 1;
        }

        info!(
            "Saved {} images for project_id={} (out of {} total, {} referenced)",
            saved_count,
            project_id,
            images.len(),
            referenced_ids.len()
        );

        Ok(saved_count)
    }

    /// List all RAG configurations
    pub fn list_configs(&self) -> Result<Vec<RAGConfigSummary>, RAGStorageError> {
        let mut summaries = Vec::new();

        if !self.base_path.exists() {
            return Ok(summaries);
        }

        let entries = fs::read_dir(&self.base_path)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let project_id = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                if project_id.is_empty() {
                    continue;
                }

                // Check if embeddings exist
                let embeddings_path = self.get_embeddings_path(&project_id);
                let has_embeddings =
                    embeddings_path.exists() && embeddings_path.join("embeddings.json").exists();

                // Load QontinuiConfig
                if let Ok(config) = self.load_qontinui_config(&project_id) {
                    // Count images in the images directory
                    let images_path = self.get_images_path(&project_id);
                    let image_count = if images_path.exists() {
                        fs::read_dir(&images_path)
                            .map(|entries| entries.filter_map(|e| e.ok()).count())
                            .unwrap_or(0)
                    } else {
                        0
                    };

                    summaries.push(RAGConfigSummary {
                        project_id: project_id.clone(),
                        project_name: config.metadata.name.clone(),
                        version: config.version.clone(),
                        exported_at: config.metadata.modified.clone(),
                        screenshot_count: image_count,
                        element_count: config.pattern_count(),
                        has_embeddings,
                        embedding_status: None,
                    });
                } else {
                    warn!("Failed to load config for project_id={}", project_id);
                }
            }
        }

        Ok(summaries)
    }

    /// Delete a RAG configuration and all associated data
    pub fn delete_config(&self, project_id: &str) -> Result<(), RAGStorageError> {
        let project_path = self.get_project_path(project_id);

        if !project_path.exists() {
            return Err(RAGStorageError::ProjectNotFound(project_id.to_string()));
        }

        fs::remove_dir_all(&project_path)?;

        info!("Deleted RAG config: project_id={}", project_id);

        Ok(())
    }

    /// Check if a project exists
    #[allow(dead_code)]
    pub fn project_exists(&self, project_id: &str) -> bool {
        self.get_config_path(project_id).exists()
    }

    /// Get storage usage statistics
    pub fn get_storage_usage(&self, project_id: &str) -> Result<StorageUsage, RAGStorageError> {
        let project_path = self.get_project_path(project_id);

        if !project_path.exists() {
            return Err(RAGStorageError::ProjectNotFound(project_id.to_string()));
        }

        let mut usage = StorageUsage {
            project_id: project_id.to_string(),
            total_bytes: 0,
            image_count: 0,
            image_bytes: 0,
            config_bytes: 0,
            embeddings_bytes: 0,
        };

        // Config file size
        let config_path = self.get_config_path(project_id);
        if config_path.exists() {
            usage.config_bytes = fs::metadata(&config_path)?.len();
            usage.total_bytes += usage.config_bytes;
        }

        // Images directory size
        let images_path = self.get_images_path(project_id);
        if images_path.exists() {
            for entry in fs::read_dir(&images_path)? {
                let entry = entry?;
                if entry.path().is_file() {
                    let size = fs::metadata(entry.path())?.len();
                    usage.image_bytes += size;
                    usage.image_count += 1;
                }
            }
        }
        usage.total_bytes += usage.image_bytes;

        // Embeddings directory size
        let embeddings_path = self.get_embeddings_path(project_id);
        if embeddings_path.exists() {
            for entry in fs::read_dir(&embeddings_path)? {
                let entry = entry?;
                if entry.path().is_file() {
                    let size = fs::metadata(entry.path())?.len();
                    usage.embeddings_bytes += size;
                }
            }
            usage.total_bytes += usage.embeddings_bytes;
        }

        Ok(usage)
    }
}

impl Default for RAGStorage {
    fn default() -> Self {
        Self::new().expect("Failed to create default RAGStorage")
    }
}

/// Storage usage statistics
#[derive(Debug, Clone)]
pub struct StorageUsage {
    pub project_id: String,
    pub total_bytes: u64,
    pub image_count: usize,
    pub image_bytes: u64,
    pub config_bytes: u64,
    pub embeddings_bytes: u64,
}

impl StorageUsage {
    /// Format bytes as human-readable string
    pub fn format_bytes(bytes: u64) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
        let mut size = bytes as f64;
        let mut unit_idx = 0;

        while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
            size /= 1024.0;
            unit_idx += 1;
        }

        format!("{:.2} {}", size, UNITS[unit_idx])
    }

    /// Get total size as human-readable string
    pub fn total_size(&self) -> String {
        Self::format_bytes(self.total_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::{ConfigMetadata, Position, State};
    use tempfile::TempDir;

    fn create_test_config() -> QontinuiConfig {
        QontinuiConfig {
            version: "2.9.0".to_string(),
            metadata: ConfigMetadata {
                name: "Test Project".to_string(),
                description: Some("A test project".to_string()),
                author: None,
                created: "2025-01-01T00:00:00Z".to_string(),
                modified: "2025-01-01T00:00:00Z".to_string(),
                tags: None,
                target_application: None,
                compatible_versions: None,
            },
            images: vec![],
            workflows: vec![],
            states: vec![State {
                id: "state1".to_string(),
                name: "Test State".to_string(),
                description: Some("A test state".to_string()),
                state_images: vec![],
                regions: None,
                locations: None,
                strings: None,
                position: Position { x: 0, y: 0 },
                entry_actions: None,
                exit_actions: None,
                timeout: None,
                is_initial: Some(true),
                is_final: None,
            }],
            transitions: vec![],
            categories: vec![],
            settings: None,
            schedules: None,
            execution_records: None,
        }
    }

    #[test]
    fn test_save_and_load_qontinui_config() {
        let temp_dir = TempDir::new().unwrap();
        let storage = RAGStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let config = create_test_config();

        // Save config
        let saved_path = storage
            .save_qontinui_config("test-project", &config)
            .unwrap();
        assert!(saved_path.exists());

        // Load config
        let loaded_config = storage.load_qontinui_config("test-project").unwrap();
        assert_eq!(loaded_config.metadata.name, config.metadata.name);
        assert_eq!(loaded_config.states.len(), 1);
    }

    #[test]
    fn test_list_configs() {
        let temp_dir = TempDir::new().unwrap();
        let storage = RAGStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let config1 = create_test_config();
        storage
            .save_qontinui_config("test-project-1", &config1)
            .unwrap();

        let mut config2 = create_test_config();
        config2.metadata.name = "Test Project 2".to_string();
        storage
            .save_qontinui_config("test-project-2", &config2)
            .unwrap();

        let summaries = storage.list_configs().unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[test]
    fn test_delete_config() {
        let temp_dir = TempDir::new().unwrap();
        let storage = RAGStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let config = create_test_config();
        storage
            .save_qontinui_config("test-project", &config)
            .unwrap();

        assert!(storage.project_exists("test-project"));

        storage.delete_config("test-project").unwrap();

        assert!(!storage.project_exists("test-project"));
    }
}
