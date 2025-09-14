use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMetadata {
    pub name: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(rename = "targetApplication")]
    pub target_application: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QontinuiConfig {
    pub version: String,
    pub metadata: ConfigMetadata,
    pub images: Vec<Value>,
    pub processes: Vec<Value>,
    pub states: Vec<Value>,
    pub transitions: Vec<Value>,
    pub settings: Option<Value>,
}

impl QontinuiConfig {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Check version
        if self.version.is_empty() {
            errors.push("Configuration version is required".to_string());
        }

        // Check for at least one state
        if self.states.is_empty() {
            errors.push("At least one state is required".to_string());
        }

        // Check metadata
        if self.metadata.name.is_empty() {
            errors.push("Configuration name is required".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "Configuration: {} (v{})\nStates: {}, Processes: {}, Transitions: {}, Images: {}",
            self.metadata.name,
            self.version,
            self.states.len(),
            self.processes.len(),
            self.transitions.len(),
            self.images.len()
        )
    }
}