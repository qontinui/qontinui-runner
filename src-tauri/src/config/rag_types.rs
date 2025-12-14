use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bounding box coordinates for UI elements
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Metadata for RAG configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGMetadata {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub created_at: String,
    pub modified_at: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_application: Option<String>,
}

/// Configuration for embedding models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub text_model: String,
    pub text_model_version: String,
    pub text_embedding_dim: u32,
    pub clip_model: String,
    pub clip_model_version: String,
    pub clip_embedding_dim: u32,
    pub dinov2_model: String,
    pub dinov2_model_version: String,
    pub dinov2_embedding_dim: u32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            text_model: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            text_model_version: "v1".to_string(),
            text_embedding_dim: 384,
            clip_model: "openai/clip-vit-base-patch32".to_string(),
            clip_model_version: "v1".to_string(),
            clip_embedding_dim: 512,
            dinov2_model: "facebook/dinov2-base".to_string(),
            dinov2_model_version: "v1".to_string(),
            dinov2_embedding_dim: 768,
        }
    }
}

/// UI element with semantic information and embeddings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGElement {
    pub id: String,
    pub name: String,
    pub element_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_text: Option<String>,
    pub bounding_box: BoundingBox,
    #[serde(default)]
    pub dominant_colors: Vec<String>,
    pub is_interactive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_action: Option<String>,
    pub state_id: String,
    pub state_name: String,
    pub is_defining_element: bool,
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f64,
    pub source_screenshot_id: String,
    pub element_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_similarity_threshold() -> f64 {
    0.85
}

/// Application state defined by UI elements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGState {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub defining_element_ids: Vec<String>,
    #[serde(default)]
    pub optional_element_ids: Vec<String>,
    #[serde(default)]
    pub expected_text: Vec<String>,
}

/// Action within a workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGAction {
    pub action_type: String,
    pub config: serde_json::Value,
}

/// Workflow containing a sequence of actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGWorkflow {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub actions: Vec<RAGAction>,
}

/// State transition triggered by a workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGTransition {
    pub id: String,
    pub name: String,
    pub from_states: Vec<String>,
    pub to_states: Vec<String>,
    pub workflow_id: String,
}

/// Information about a captured screenshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotInfo {
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub captured_at: String,
}

/// Information about the vector database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDBInfo {
    pub filename: String,
    pub element_count: u32,
    pub last_indexed_at: String,
    pub index_hash: String,
}

/// Main RAG configuration container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RAGConfig {
    pub version: String,
    pub config_type: String,
    pub metadata: RAGMetadata,
    pub embedding_config: EmbeddingConfig,
    #[serde(default)]
    pub elements: Vec<RAGElement>,
    #[serde(default)]
    pub states: Vec<RAGState>,
    #[serde(default)]
    pub workflows: Vec<RAGWorkflow>,
    #[serde(default)]
    pub transitions: Vec<RAGTransition>,
    #[serde(default)]
    pub screenshots: HashMap<String, ScreenshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_db: Option<VectorDBInfo>,
}

impl RAGConfig {
    /// Create a new RAG configuration with default values
    pub fn new(name: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            version: "1.0.0".to_string(),
            config_type: "rag".to_string(),
            metadata: RAGMetadata {
                name,
                description: None,
                author: None,
                created_at: now.clone(),
                modified_at: now,
                tags: Vec::new(),
                target_application: None,
            },
            embedding_config: EmbeddingConfig::default(),
            elements: Vec::new(),
            states: Vec::new(),
            workflows: Vec::new(),
            transitions: Vec::new(),
            screenshots: HashMap::new(),
            vector_db: None,
        }
    }

    /// Validate the RAG configuration
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Check version
        if self.version.is_empty() {
            errors.push("Configuration version is required".to_string());
        }

        // Check config type
        if self.config_type != "rag" {
            errors.push(format!(
                "Invalid config_type '{}', expected 'rag'",
                self.config_type
            ));
        }

        // Check metadata
        if self.metadata.name.is_empty() {
            errors.push("Configuration name is required".to_string());
        }

        // Check that all state_ids in elements exist
        let state_ids: std::collections::HashSet<_> =
            self.states.iter().map(|s| s.id.as_str()).collect();
        for element in &self.elements {
            if !state_ids.contains(element.state_id.as_str()) {
                errors.push(format!(
                    "Element '{}' references non-existent state '{}'",
                    element.id, element.state_id
                ));
            }
        }

        // Check that all workflow_ids in transitions exist
        let workflow_ids: std::collections::HashSet<_> =
            self.workflows.iter().map(|w| w.id.as_str()).collect();
        for transition in &self.transitions {
            if !workflow_ids.contains(transition.workflow_id.as_str()) {
                errors.push(format!(
                    "Transition '{}' references non-existent workflow '{}'",
                    transition.id, transition.workflow_id
                ));
            }
        }

        // Check that defining elements exist
        let element_ids: std::collections::HashSet<_> =
            self.elements.iter().map(|e| e.id.as_str()).collect();
        for state in &self.states {
            for element_id in &state.defining_element_ids {
                if !element_ids.contains(element_id.as_str()) {
                    errors.push(format!(
                        "State '{}' references non-existent defining element '{}'",
                        state.id, element_id
                    ));
                }
            }
            for element_id in &state.optional_element_ids {
                if !element_ids.contains(element_id.as_str()) {
                    errors.push(format!(
                        "State '{}' references non-existent optional element '{}'",
                        state.id, element_id
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Get a summary of the configuration
    pub fn summary(&self) -> String {
        format!(
            "RAG Configuration: {} (v{})\nElements: {}, States: {}, Workflows: {}, Transitions: {}, Screenshots: {}",
            self.metadata.name,
            self.version,
            self.elements.len(),
            self.states.len(),
            self.workflows.len(),
            self.transitions.len(),
            self.screenshots.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rag_config_new() {
        let config = RAGConfig::new("Test Config".to_string());
        assert_eq!(config.version, "1.0.0");
        assert_eq!(config.config_type, "rag");
        assert_eq!(config.metadata.name, "Test Config");
    }

    #[test]
    fn test_rag_config_validate_empty() {
        let config = RAGConfig::new("Test".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_rag_config_validate_invalid_config_type() {
        let mut config = RAGConfig::new("Test".to_string());
        config.config_type = "invalid".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.text_embedding_dim, 384);
        assert_eq!(config.clip_embedding_dim, 512);
        assert_eq!(config.dinov2_embedding_dim, 768);
    }

    #[test]
    fn test_bounding_box_serialization() {
        let bbox = BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        let json = serde_json::to_string(&bbox).unwrap();
        let deserialized: BoundingBox = serde_json::from_str(&json).unwrap();
        assert_eq!(bbox, deserialized);
    }
}
