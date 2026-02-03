//! RAG (Retrieval-Augmented Generation) module
//!
//! This module handles importing and managing RAG configurations for visual automation.
//! RAG configs include pattern screenshots, element metadata, and embeddings for semantic search.
//!
//! # Storage Structure
//!
//! RAG data is stored in the user's home directory:
//! ```text
//! ~/.qontinui/rag/
//!   ├── {project_id}/
//!   │   ├── config.json              # Full QontinuiConfig (source of truth)
//!   │   ├── screenshots/             # Pattern screenshots (extracted from images[])
//!   │   │   ├── {image_id}.png
//!   │   │   └── ...
//!   │   └── embeddings/              # Generated embeddings
//!   │       ├── embeddings.json
//!   │       └── index.bin
//!   └── ...
//! ```
//!
//! # Design Philosophy
//!
//! The runner accepts the full `QontinuiConfig` format directly from the frontend.
//! This avoids format transformation and ensures consistency with the Python library.
//! The runner stores the full config as-is and extracts what it needs internally.
//!
//! # Workflow
//!
//! 1. Web app exports QontinuiConfig JSON
//! 2. Runner receives import request via API (accepts QontinuiConfig directly)
//! 3. Runner saves full config to ~/.qontinui/rag/{project_id}/config.json
//! 4. Runner extracts and saves images from config.images[]
//! 5. Runner triggers Python embedding generation (background process)
//! 6. Embeddings are generated for patterns referenced in states[].stateImages[]
//! 7. Runner can search RAG data by project_id + query

pub mod embeddings;
pub mod find;
pub mod search;
pub mod storage;

pub use embeddings::{EmbeddingGenerator, EmbeddingStatus};
pub use find::FindError;
pub use search::{SearchError, SemanticSearch};
pub use storage::RAGStorage;

use crate::config::qontinui_config::StateDescription;
use crate::config::Category;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Full QontinuiConfig format - matches the TypeScript/Python export schema.
/// Uses serde_json::Value for complex nested structures to avoid duplicating
/// all the type definitions from export-schema.ts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QontinuiConfig {
    /// Schema version (e.g., "2.6.0")
    pub version: String,

    /// Project metadata (name, description, created, modified, etc.)
    pub metadata: ConfigMetadata,

    /// Image assets - base64 encoded images referenced by patterns
    #[serde(default)]
    pub images: Vec<ImageAsset>,

    /// Workflows - graph-format automation workflows
    #[serde(default)]
    pub workflows: Vec<serde_json::Value>,

    /// States - contain stateImages with patterns for RAG
    #[serde(default)]
    pub states: Vec<State>,

    /// Transitions between states
    #[serde(default)]
    pub transitions: Vec<serde_json::Value>,

    /// Workflow categories
    #[serde(default)]
    pub categories: Vec<Category>,

    /// Optional settings
    #[serde(default)]
    pub settings: Option<serde_json::Value>,

    /// Optional schedules
    #[serde(default)]
    pub schedules: Option<Vec<serde_json::Value>>,

    /// Optional execution records
    #[serde(default, rename = "executionRecords")]
    pub execution_records: Option<Vec<serde_json::Value>>,
}

/// Configuration metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMetadata {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    pub created: String,
    pub modified: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default, rename = "targetApplication")]
    pub target_application: Option<String>,
    #[serde(default, rename = "compatibleVersions")]
    pub compatible_versions: Option<serde_json::Value>,
}

/// Image asset with base64 data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAsset {
    pub id: String,
    pub name: String,
    pub data: String, // Base64 encoded
    pub format: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub hash: Option<String>,
}

/// State containing stateImages for RAG processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "stateImages")]
    pub state_images: Vec<StateImage>,
    #[serde(default)]
    pub regions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub locations: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub strings: Option<Vec<serde_json::Value>>,
    pub position: Position,
    #[serde(default, rename = "entryActions")]
    pub entry_actions: Option<Vec<String>>,
    #[serde(default, rename = "exitActions")]
    pub exit_actions: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default, rename = "isInitial")]
    pub is_initial: Option<bool>,
    #[serde(default, rename = "isFinal")]
    pub is_final: Option<bool>,
    /// Rich description for AI verification agent
    #[serde(skip_serializing_if = "Option::is_none", rename = "aiDescription")]
    pub ai_description: Option<StateDescription>,
}

/// Position coordinates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

/// StateImage containing patterns - the core of RAG indexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateImage {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub patterns: Vec<Pattern>,
    #[serde(default)]
    pub shared: bool,
    #[serde(default)]
    pub probability: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, rename = "searchRegions")]
    pub search_regions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub monitors: Option<Vec<i32>>,
}

/// Pattern - references an image for template matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "imageId")]
    pub image_id: String,
    #[serde(default)]
    pub mask: Option<String>,
    #[serde(default, rename = "searchRegions")]
    pub search_regions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub fixed: bool,
    #[serde(default)]
    pub similarity: Option<f64>,
    #[serde(default, rename = "targetPosition")]
    pub target_position: Option<serde_json::Value>,
    #[serde(default, rename = "offsetX")]
    pub offset_x: Option<i32>,
    #[serde(default, rename = "offsetY")]
    pub offset_y: Option<i32>,
}

impl QontinuiConfig {
    /// Get project ID (derived from metadata name or generated)
    pub fn project_id(&self) -> String {
        // Use metadata name as project ID, sanitized for filesystem
        self.metadata
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .to_lowercase()
    }

    /// Count total StateImages across all states
    pub fn state_image_count(&self) -> usize {
        self.states.iter().map(|s| s.state_images.len()).sum()
    }

    /// Count total patterns across all StateImages
    pub fn pattern_count(&self) -> usize {
        self.states
            .iter()
            .flat_map(|s| &s.state_images)
            .map(|si| si.patterns.len())
            .sum()
    }

    /// Get all unique image IDs referenced by patterns
    pub fn referenced_image_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .states
            .iter()
            .flat_map(|s| &s.state_images)
            .flat_map(|si| &si.patterns)
            .map(|p| p.image_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// RAG configuration summary (for listing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGConfigSummary {
    pub project_id: String,
    pub project_name: String,
    pub version: String,
    pub exported_at: String,
    pub screenshot_count: usize,
    pub element_count: usize,
    pub has_embeddings: bool,
    pub embedding_status: Option<String>,
}

/// Result of RAG import operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub success: bool,
    pub project_id: String,
    pub message: String,
    pub screenshot_count: usize,
    pub element_count: usize,
    pub storage_path: String,
}

/// Bounding box coordinates for search results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Search filters for RAG search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Filter by element type
    pub element_type: Option<String>,

    /// Filter by state ID
    pub state_id: Option<String>,

    /// Maximum number of results
    pub limit: Option<usize>,

    /// Minimum similarity threshold (0.0 - 1.0)
    pub min_similarity: Option<f32>,
}

/// Search result from RAG query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub element_id: String,
    pub label: String,
    pub screenshot_id: String,
    pub bbox: BoundingBox,
    pub similarity: f32,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rag_config_with_categories() {
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "categories": [
                {"name": "Main", "automationEnabled": true},
                {"name": "Incoming Transitions", "automationEnabled": false}
            ]
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.categories.len(), 2);
        assert_eq!(config.categories[0].name, "Main");
        assert!(config.categories[0].automation_enabled);
        assert_eq!(config.categories[1].name, "Incoming Transitions");
        assert!(!config.categories[1].automation_enabled);
    }
}
