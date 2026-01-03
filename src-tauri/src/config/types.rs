use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Category in a configuration.
/// Categories organize workflows and control which are available for automation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Category {
    pub name: String,
    #[serde(default = "default_automation_enabled", rename = "automationEnabled")]
    pub automation_enabled: bool,
}

fn default_automation_enabled() -> bool {
    true
}

/// Custom deserializer for categories that handles both string and object formats.
/// - String format: `["Main", "Testing"]` - converts to Category with automation_enabled=true
/// - Object format: `[{"name": "Main", "automationEnabled": true}]` - deserializes directly
pub fn deserialize_categories<'de, D>(deserializer: D) -> Result<Vec<Category>, D::Error>
where
    D: Deserializer<'de>,
{
    let values: Vec<Value> = Vec::deserialize(deserializer)?;
    let mut categories = Vec::with_capacity(values.len());

    for value in values {
        let category = match value {
            Value::String(s) => Category {
                name: s,
                automation_enabled: true,
            },
            Value::Object(_) => serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            _ => {
                return Err(serde::de::Error::custom(
                    "Category must be a string or object",
                ))
            }
        };
        categories.push(category);
    }

    Ok(categories)
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
    /// Project ID for qontinui-web execution run reporting
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QontinuiConfig {
    pub version: String,
    pub metadata: ConfigMetadata,
    pub images: Vec<Value>,
    pub workflows: Vec<Value>,
    pub states: Vec<Value>,
    pub transitions: Vec<Value>,
    #[serde(default, deserialize_with = "deserialize_categories")]
    pub categories: Vec<Category>,
    pub settings: Option<Settings>,
    /// AI contexts stored with the project configuration
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<Value>,
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
            "Configuration: {} (v{})\nStates: {}, Workflows: {}, Transitions: {}, Images: {}, Categories: {}, Contexts: {}",
            self.metadata.name,
            self.version,
            self.states.len(),
            self.workflows.len(),
            self.transitions.len(),
            self.images.len(),
            self.categories.len(),
            self.contexts.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_category() {
        let json = r#"{"name": "Main", "automationEnabled": true}"#;
        let category: Category = serde_json::from_str(json).unwrap();
        assert_eq!(category.name, "Main");
        assert!(category.automation_enabled);

        // Test with automationEnabled=false
        let json = r#"{"name": "Incoming Transitions", "automationEnabled": false}"#;
        let category: Category = serde_json::from_str(json).unwrap();
        assert_eq!(category.name, "Incoming Transitions");
        assert!(!category.automation_enabled);

        // Test default automationEnabled (true)
        let json = r#"{"name": "Test"}"#;
        let category: Category = serde_json::from_str(json).unwrap();
        assert_eq!(category.name, "Test");
        assert!(category.automation_enabled);
    }

    #[test]
    fn test_deserialize_config_with_categories() {
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test Project",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "images": [],
            "workflows": [],
            "states": [],
            "transitions": [],
            "categories": [
                {"name": "Main", "automationEnabled": true},
                {"name": "Incoming Transitions", "automationEnabled": false}
            ]
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.version, "2.0.0");
        assert_eq!(config.categories.len(), 2);
        assert_eq!(config.categories[0].name, "Main");
        assert!(config.categories[0].automation_enabled);
        assert_eq!(config.categories[1].name, "Incoming Transitions");
        assert!(!config.categories[1].automation_enabled);
    }
}
