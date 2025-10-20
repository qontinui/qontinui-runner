use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    #[default]
    Real,
    Mock,
    Screenshot,
}

impl ExecutionMode {
    pub fn as_str(&self) -> &str {
        match self {
            ExecutionMode::Real => "real",
            ExecutionMode::Mock => "mock",
            ExecutionMode::Screenshot => "screenshot",
        }
    }

    pub fn is_mock(&self) -> bool {
        matches!(self, ExecutionMode::Mock)
    }

    pub fn is_screenshot(&self) -> bool {
        matches!(self, ExecutionMode::Screenshot)
    }

    #[allow(dead_code)]
    pub fn is_real(&self) -> bool {
        matches!(self, ExecutionMode::Real)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSettings {
    #[serde(default)]
    pub default_timeout: Option<u64>,
    #[serde(default)]
    pub default_retry_count: Option<u32>,
    #[serde(default)]
    pub action_delay: Option<u64>,
    #[serde(default)]
    pub failure_strategy: Option<String>,
    #[serde(default)]
    pub headless: Option<bool>,
    #[serde(default, rename = "useGraphExecution")]
    pub use_graph_execution: Option<bool>,
    #[serde(default, rename = "executionMode")]
    pub execution_mode: Option<ExecutionMode>,
    #[serde(default, rename = "screenshotDirectory")]
    pub screenshot_directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub execution: Option<ExecutionSettings>,
    #[serde(default)]
    pub recognition: Option<Value>,
    #[serde(default)]
    pub logging: Option<Value>,
    #[serde(default)]
    pub performance: Option<Value>,
}

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
    pub workflows: Vec<Value>,
    pub states: Vec<Value>,
    pub transitions: Vec<Value>,
    pub categories: Vec<String>,
    pub settings: Option<Settings>,
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
            "Configuration: {} (v{})\nStates: {}, Workflows: {}, Transitions: {}, Images: {}, Categories: {}",
            self.metadata.name,
            self.version,
            self.states.len(),
            self.workflows.len(),
            self.transitions.len(),
            self.images.len(),
            self.categories.len()
        )
    }

    pub fn get_execution_mode(&self) -> ExecutionMode {
        self.settings
            .as_ref()
            .and_then(|s| s.execution.as_ref())
            .and_then(|e| e.execution_mode.clone())
            .unwrap_or_default()
    }

    pub fn get_screenshot_directory(&self) -> Option<String> {
        self.settings
            .as_ref()
            .and_then(|s| s.execution.as_ref())
            .and_then(|e| e.screenshot_directory.clone())
    }

    pub fn is_mock_mode(&self) -> bool {
        self.get_execution_mode().is_mock()
    }

    pub fn is_screenshot_mode(&self) -> bool {
        self.get_execution_mode().is_screenshot()
    }

    #[allow(dead_code)]
    pub fn is_real_mode(&self) -> bool {
        self.get_execution_mode().is_real()
    }
}
