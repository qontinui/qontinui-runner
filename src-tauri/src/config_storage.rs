//! Configuration Storage
//!
//! This module provides persistent storage for Qontinui configurations with
//! import/export capabilities. Configurations are stored on disk with metadata
//! for easy management.
//!
//! # Storage Structure
//!
//! ```text
//! ~/.config/com.qontinui.runner/configs/
//!   +-- {id}.json              # Full configuration file
//!   +-- metadata.json          # Index of all stored configs
//! ```

use crate::config::ConfigLoader;
use crate::config::QontinuiConfig;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{info, warn};

/// Errors that can occur during config storage operations
#[derive(Debug, Error)]
pub enum ConfigStorageError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Config not found: {0}")]
    NotFound(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Parse error: {0}")]
    ParseError(String),
}

/// Metadata for a stored configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMetadata {
    /// Unique identifier for this config
    pub id: String,
    /// User-friendly name
    pub name: String,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Source file path (if imported from file)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Config version from the file
    pub version: String,
    /// Number of states in the config
    pub state_count: usize,
    /// Number of workflows in the config
    pub workflow_count: usize,
    /// Number of transitions in the config
    pub transition_count: usize,
    /// When this config was imported/created
    pub created_at: String,
    /// When this config was last updated
    pub updated_at: String,
}

/// A stored configuration with full data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredConfig {
    /// Metadata about this config
    pub metadata: ConfigMetadata,
    /// The full configuration data
    pub config: QontinuiConfig,
}

/// Index of all stored configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigIndex {
    /// Map of config ID to metadata
    configs: HashMap<String, ConfigMetadata>,
}

/// Storage for multiple configuration objects.
///
/// Provides persistent storage with import/export capabilities and metadata tracking.
#[derive(Debug)]
pub struct ConfigStorage {
    /// Base directory for config storage
    base_path: PathBuf,
    /// In-memory index of configs
    index: ConfigIndex,
}

impl Default for ConfigStorage {
    fn default() -> Self {
        Self::new().expect("Failed to create default ConfigStorage")
    }
}

impl ConfigStorage {
    /// Create a new ConfigStorage with default path.
    ///
    /// Scoped per-runner for secondary instances so themed runners don't
    /// share their config library with the primary.
    pub fn new() -> Result<Self, ConfigStorageError> {
        let base = dirs::config_dir()
            .ok_or_else(|| {
                ConfigStorageError::InvalidPath("Could not determine config directory".to_string())
            })?
            .join("com.qontinui.runner");
        let config_dir = crate::instance::scope_path(&base).join("configs");

        // Create directory if it doesn't exist
        fs::create_dir_all(&config_dir)?;

        let mut storage = Self {
            base_path: config_dir,
            index: ConfigIndex::default(),
        };

        // Load existing index
        storage.load_index()?;

        info!("Config storage initialized at: {:?}", storage.base_path);

        Ok(storage)
    }

    /// Create a degraded ConfigStorage that will return errors on use
    pub fn new_degraded() -> Self {
        warn!("Creating degraded ConfigStorage - config storage features will be disabled");
        Self {
            base_path: PathBuf::new(),
            index: ConfigIndex::default(),
        }
    }

    /// Check if this is a degraded instance
    pub fn is_degraded(&self) -> bool {
        self.base_path.as_os_str().is_empty()
    }

    /// Get the path for the index file
    fn index_path(&self) -> PathBuf {
        self.base_path.join("metadata.json")
    }

    /// Get the path for a config file by ID
    fn config_path(&self, id: &str) -> PathBuf {
        self.base_path.join(format!("{}.json", id))
    }

    /// Load the index from disk
    fn load_index(&mut self) -> Result<(), ConfigStorageError> {
        let index_path = self.index_path();
        if index_path.exists() {
            let content = fs::read_to_string(&index_path)?;
            self.index = serde_json::from_str(&content)?;
            info!(
                "Loaded config index with {} entries",
                self.index.configs.len()
            );
        }
        Ok(())
    }

    /// Save the index to disk
    fn save_index(&self) -> Result<(), ConfigStorageError> {
        let index_path = self.index_path();
        let content = serde_json::to_string_pretty(&self.index)?;
        fs::write(&index_path, content)?;
        Ok(())
    }

    /// Generate a unique ID for a new config
    fn generate_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Import a configuration from a file
    pub fn import_from_file(
        &mut self,
        path: &str,
        name: &str,
    ) -> Result<String, ConfigStorageError> {
        if self.is_degraded() {
            return Err(ConfigStorageError::InvalidPath(
                "Config storage is degraded".to_string(),
            ));
        }

        // Load and validate the config
        let config = ConfigLoader::load_from_file(path).map_err(ConfigStorageError::ParseError)?;

        let id = self.generate_id();
        let now = Utc::now().to_rfc3339();

        let metadata = ConfigMetadata {
            id: id.clone(),
            name: name.to_string(),
            description: config.metadata.description.clone(),
            source_path: Some(path.to_string()),
            version: config.version.clone(),
            state_count: config.states.len(),
            workflow_count: config.workflows.len(),
            transition_count: config.transitions.len(),
            created_at: now.clone(),
            updated_at: now,
        };

        // Save the config file
        let config_path = self.config_path(&id);
        let stored = StoredConfig {
            metadata: metadata.clone(),
            config,
        };
        let content = serde_json::to_string_pretty(&stored)?;
        fs::write(&config_path, content)?;

        // Update index
        self.index.configs.insert(id.clone(), metadata);
        self.save_index()?;

        info!("Imported config '{}' from {:?} with id {}", name, path, id);

        Ok(id)
    }

    /// Export a configuration to a file
    pub fn export_to_file(&self, id: &str, path: &str) -> Result<(), ConfigStorageError> {
        if self.is_degraded() {
            return Err(ConfigStorageError::InvalidPath(
                "Config storage is degraded".to_string(),
            ));
        }

        let stored = self.get(id)?;

        // Export just the config, not the metadata wrapper
        let content = serde_json::to_string_pretty(&stored.config)?;
        fs::write(path, content)?;

        info!("Exported config {} to {:?}", id, path);

        Ok(())
    }

    /// Get a stored configuration by ID
    pub fn get(&self, id: &str) -> Result<StoredConfig, ConfigStorageError> {
        if self.is_degraded() {
            return Err(ConfigStorageError::InvalidPath(
                "Config storage is degraded".to_string(),
            ));
        }

        if !self.index.configs.contains_key(id) {
            return Err(ConfigStorageError::NotFound(id.to_string()));
        }

        let config_path = self.config_path(id);
        if !config_path.exists() {
            return Err(ConfigStorageError::NotFound(id.to_string()));
        }

        let content = fs::read_to_string(&config_path)?;
        let stored: StoredConfig = serde_json::from_str(&content)?;

        Ok(stored)
    }

    /// List all stored configurations (metadata only)
    pub fn list(&self) -> Vec<ConfigMetadata> {
        self.index.configs.values().cloned().collect()
    }

    /// Update config metadata
    pub fn update_metadata(
        &mut self,
        id: &str,
        name: Option<String>,
        description: Option<String>,
    ) -> Result<(), ConfigStorageError> {
        if self.is_degraded() {
            return Err(ConfigStorageError::InvalidPath(
                "Config storage is degraded".to_string(),
            ));
        }

        // Check if config exists
        if !self.index.configs.contains_key(id) {
            return Err(ConfigStorageError::NotFound(id.to_string()));
        }

        // Load the stored config
        let config_path = self.config_path(id);
        let content = fs::read_to_string(&config_path)?;
        let mut stored: StoredConfig = serde_json::from_str(&content)?;

        // Update metadata
        if let Some(new_name) = name {
            stored.metadata.name = new_name;
        }
        if description.is_some() {
            stored.metadata.description = description;
        }
        stored.metadata.updated_at = Utc::now().to_rfc3339();

        // Save back
        let content = serde_json::to_string_pretty(&stored)?;
        fs::write(&config_path, content)?;

        // Update index
        self.index.configs.insert(id.to_string(), stored.metadata);
        self.save_index()?;

        info!("Updated metadata for config {}", id);

        Ok(())
    }

    /// Delete a stored configuration
    pub fn delete(&mut self, id: &str) -> Result<(), ConfigStorageError> {
        if self.is_degraded() {
            return Err(ConfigStorageError::InvalidPath(
                "Config storage is degraded".to_string(),
            ));
        }

        if !self.index.configs.contains_key(id) {
            return Err(ConfigStorageError::NotFound(id.to_string()));
        }

        // Remove the config file
        let config_path = self.config_path(id);
        if config_path.exists() {
            fs::remove_file(&config_path)?;
        }

        // Update index
        self.index.configs.remove(id);
        self.save_index()?;

        info!("Deleted config {}", id);

        Ok(())
    }

    // ========================================================================
    // Legacy API compatibility methods
    // ========================================================================

    /// Check if a configuration exists
    #[allow(dead_code)]
    pub fn exists(&self, id: &str) -> bool {
        self.index.configs.contains_key(id)
    }

    /// Get metadata for a config
    #[allow(dead_code)]
    pub fn get_metadata(&self, id: &str) -> Option<&ConfigMetadata> {
        self.index.configs.get(id)
    }

    /// Store a configuration with the given ID (legacy in-memory style)
    /// This imports the config and stores it persistently
    #[allow(dead_code)]
    pub fn store(&mut self, id: String, config: QontinuiConfig) {
        let now = Utc::now().to_rfc3339();
        let metadata = ConfigMetadata {
            id: id.clone(),
            name: config.metadata.name.clone(),
            description: config.metadata.description.clone(),
            source_path: None,
            version: config.version.clone(),
            state_count: config.states.len(),
            workflow_count: config.workflows.len(),
            transition_count: config.transitions.len(),
            created_at: now.clone(),
            updated_at: now,
        };

        let stored = StoredConfig {
            metadata: metadata.clone(),
            config,
        };

        // Try to save to disk
        if !self.is_degraded() {
            let config_path = self.config_path(&id);
            if let Ok(content) = serde_json::to_string_pretty(&stored) {
                if fs::write(&config_path, content).is_ok() {
                    self.index.configs.insert(id.clone(), metadata);
                    let _ = self.save_index();
                    info!("Stored configuration with id: {}", id);
                    return;
                }
            }
        }

        warn!("Failed to persist config {}, storage may be degraded", id);
    }

    /// Remove a configuration by ID (legacy compatibility)
    #[allow(dead_code)]
    pub fn remove(&mut self, id: &str) -> Option<QontinuiConfig> {
        if let Ok(stored) = self.get(id) {
            let _ = self.delete(id);
            Some(stored.config)
        } else {
            None
        }
    }

    /// List all stored configuration IDs (legacy compatibility)
    #[allow(dead_code)]
    pub fn list_ids(&self) -> Vec<String> {
        self.index.configs.keys().cloned().collect()
    }

    /// Check if a configuration exists (legacy compatibility alias)
    #[allow(dead_code)]
    pub fn contains(&self, id: &str) -> bool {
        self.exists(id)
    }

    /// Clear all stored configurations
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        if self.is_degraded() {
            return;
        }

        info!("Clearing all stored configurations");
        let ids: Vec<String> = self.index.configs.keys().cloned().collect();
        for id in ids {
            let _ = self.delete(&id);
        }
    }

    /// Get the count of stored configurations
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.index.configs.len()
    }

    /// Check if storage is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.index.configs.is_empty()
    }

    /// Get summaries of all stored configurations (legacy compatibility)
    #[allow(dead_code)]
    pub fn get_summaries(&self) -> Vec<ConfigSummary> {
        self.list()
            .into_iter()
            .map(|m| ConfigSummary {
                id: m.id,
                name: m.name,
                workflow_count: m.workflow_count,
            })
            .collect()
    }
}

/// Serializable summary of a stored configuration (legacy compatibility)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSummary {
    pub id: String,
    pub name: String,
    pub workflow_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_storage() -> (ConfigStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().join("configs");
        fs::create_dir_all(&base_path).unwrap();

        let storage = ConfigStorage {
            base_path,
            index: ConfigIndex::default(),
        };

        (storage, temp_dir)
    }

    fn create_test_config_file(temp_dir: &TempDir) -> PathBuf {
        let config_json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test Config",
                "description": "A test configuration",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "images": [],
            "workflows": [{"id": "workflow1", "name": "Test Workflow"}],
            "states": [{"id": "state1", "name": "Initial State"}],
            "transitions": [],
            "categories": []
        }"#;

        let config_path = temp_dir.path().join("test_config.json");
        fs::write(&config_path, config_json).unwrap();
        config_path
    }

    #[test]
    fn test_import_and_get() {
        let (mut storage, temp_dir) = create_test_storage();
        let config_path = create_test_config_file(&temp_dir);

        let id = storage
            .import_from_file(config_path.to_str().unwrap(), "My Test Config")
            .unwrap();

        let stored = storage.get(&id).unwrap();
        assert_eq!(stored.metadata.name, "My Test Config");
        assert_eq!(stored.metadata.version, "2.0.0");
        assert_eq!(stored.metadata.state_count, 1);
        assert_eq!(stored.metadata.workflow_count, 1);
    }

    #[test]
    fn test_list_configs() {
        let (mut storage, temp_dir) = create_test_storage();
        let config_path = create_test_config_file(&temp_dir);

        storage
            .import_from_file(config_path.to_str().unwrap(), "Config 1")
            .unwrap();
        storage
            .import_from_file(config_path.to_str().unwrap(), "Config 2")
            .unwrap();

        let list = storage.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_update_metadata() {
        let (mut storage, temp_dir) = create_test_storage();
        let config_path = create_test_config_file(&temp_dir);

        let id = storage
            .import_from_file(config_path.to_str().unwrap(), "Original Name")
            .unwrap();

        storage
            .update_metadata(
                &id,
                Some("New Name".to_string()),
                Some("New description".to_string()),
            )
            .unwrap();

        let stored = storage.get(&id).unwrap();
        assert_eq!(stored.metadata.name, "New Name");
        assert_eq!(
            stored.metadata.description,
            Some("New description".to_string())
        );
    }

    #[test]
    fn test_delete() {
        let (mut storage, temp_dir) = create_test_storage();
        let config_path = create_test_config_file(&temp_dir);

        let id = storage
            .import_from_file(config_path.to_str().unwrap(), "To Delete")
            .unwrap();

        assert!(storage.exists(&id));
        storage.delete(&id).unwrap();
        assert!(!storage.exists(&id));
    }

    #[test]
    fn test_export() {
        let (mut storage, temp_dir) = create_test_storage();
        let config_path = create_test_config_file(&temp_dir);

        let id = storage
            .import_from_file(config_path.to_str().unwrap(), "To Export")
            .unwrap();

        let export_path = temp_dir.path().join("exported.json");
        storage
            .export_to_file(&id, export_path.to_str().unwrap())
            .unwrap();

        assert!(export_path.exists());
        let content = fs::read_to_string(&export_path).unwrap();
        let config: QontinuiConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.metadata.name, "Test Config");
    }
}
