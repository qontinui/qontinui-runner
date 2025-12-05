use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

/// Screen selection for screenshot capture
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ScreenSelection {
    /// Capture all screens
    All,
    /// Capture primary screen only
    Primary,
    /// Capture specific screens by index
    Specific { indices: Vec<u32> },
}

/// Settings for screenshot capture tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotCaptureSettings {
    /// Whether screenshot capture is enabled
    pub enabled: bool,
    /// Whether manual click capture is enabled
    #[serde(rename = "manualClicksEnabled")]
    pub manual_clicks_enabled: bool,
    /// Output folder for screenshots
    #[serde(rename = "outputFolder")]
    pub output_folder: String,
    /// Base name for screenshot files
    #[serde(rename = "baseImageName")]
    pub base_image_name: String,
    /// Which screens to capture
    pub screens: ScreenSelection,
    /// Capture timings in milliseconds (delays after click)
    #[serde(rename = "captureTimings")]
    pub capture_timings: Vec<u32>,
}
