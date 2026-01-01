//! QontinuiConfig types
//!
//! These types mirror the Python/TypeScript QontinuiConfig format,
//! which is the source of truth for automation configurations.
//!
//! The runner accepts this format directly on /rag/import - no transformation needed.

use super::types::{deserialize_categories, Category};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Description types for AI Verification Agent
// =============================================================================
//
// These types provide rich descriptions for states, actions, transitions, and
// workflows that enable an AI verification agent to explore applications using
// the state machine structure and verify that reality matches expectations.

/// Rich description for a state in the state machine.
///
/// Provides context for an AI verification agent to understand what a state
/// represents and how to verify it matches expectations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StateDescription {
    /// Brief description of the state (1-2 sentences)
    pub summary: String,
    /// UI elements that should be visible in this state (e.g., "Login button", "Username field")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_elements: Option<Vec<String>>,
    /// UI elements that should NOT be visible in this state - helps detect error dialogs or wrong states
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unexpected_elements: Option<Vec<String>>,
    /// Business context - what the user is trying to accomplish when in this state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_goal: Option<String>,
    /// Custom hints for AI verification - specific things to check or look for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_prompt: Option<String>,
}

/// Rich description for an action in a workflow.
///
/// Provides context for an AI verification agent to understand what an action
/// is supposed to accomplish and how to verify it succeeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ActionDescription {
    /// What this action is supposed to accomplish (e.g., "Click the submit button to proceed")
    pub intent: String,
    /// Conditions that should be true before this action executes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<Vec<String>>,
    /// Conditions that should be true after this action completes successfully
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postconditions: Option<Vec<String>>,
    /// Known ways this action can fail (e.g., "Button may be disabled", "Network timeout")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_modes: Option<Vec<String>>,
}

/// Rich description for a transition between states.
///
/// Provides context for an AI verification agent to understand what a transition
/// accomplishes and how to verify it completed successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TransitionDescription {
    /// What this transition accomplishes (e.g., "Navigate from login to dashboard")
    pub intent: String,
    /// Conditions that must be true before this transition can occur
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<Vec<String>>,
    /// Conditions that should be true after this transition completes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postconditions: Option<Vec<String>>,
    /// Known ways this transition can fail (e.g., "Authentication error", "Server unavailable")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_modes: Option<Vec<String>>,
    /// Typical duration of this transition in milliseconds - helps detect performance issues
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_duration_ms: Option<u64>,
}

/// Rich description for an entire workflow.
///
/// Provides high-level context for an AI verification agent to understand
/// the overall purpose and success criteria of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowDescription {
    /// Overall goal of the workflow (e.g., "Complete user registration process")
    pub purpose: String,
    /// How to determine if the workflow succeeded (e.g., "User sees welcome message")
    pub success_criteria: String,
    /// Human-readable summary of the workflow steps for quick understanding
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps_summary: Option<Vec<String>>,
    /// Why this workflow exists and its importance in the application
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_context: Option<String>,
}

/// Complete Qontinui configuration - mirrors Python/TypeScript QontinuiConfig.
/// This is the canonical format exported from qontinui-web.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QontinuiConfig {
    pub version: String,
    pub metadata: ConfigMetadata,
    #[serde(default)]
    pub images: Vec<ImageAsset>,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default)]
    pub states: Vec<State>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    #[serde(default, deserialize_with = "deserialize_categories")]
    pub categories: Vec<Category>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<ConfigSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedules: Option<Vec<Schedule>>,
}

#[allow(dead_code)]
impl QontinuiConfig {
    /// Get project_id from metadata name (slugified)
    pub fn project_id(&self) -> String {
        self.metadata
            .name
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
            .trim_matches('-')
            .to_string()
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
}

/// Configuration metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMetadata {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub created: String,
    pub modified: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "targetApplication")]
    pub target_application: Option<String>,
    /// Project ID from qontinui-web for test run reporting
    #[serde(skip_serializing_if = "Option::is_none", rename = "projectId")]
    pub project_id: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Workflow definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub format: String,
    pub version: String,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub connections: HashMap<String, ActionOutputs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<WorkflowMetadata>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "initialStateIds")]
    pub initial_state_ids: Option<Vec<String>>,
    /// Rich description for AI verification agent
    #[serde(skip_serializing_if = "Option::is_none", rename = "aiDescription")]
    pub ai_description: Option<WorkflowDescription>,
}

/// Action in a workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "retryCount")]
    pub retry_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "continueOnError")]
    pub continue_on_error: Option<bool>,
    /// Rich description for AI verification agent
    #[serde(skip_serializing_if = "Option::is_none", rename = "aiDescription")]
    pub ai_description: Option<ActionDescription>,
}

/// Connection between actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub action: String,
    #[serde(rename = "type")]
    pub connection_type: String,
    pub index: u32,
}

/// Action outputs/connections
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActionOutputs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<Vec<Vec<Connection>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<Vec<Vec<Connection>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Vec<Vec<Connection>>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "true")]
    pub true_branch: Option<Vec<Vec<Connection>>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "false")]
    pub false_branch: Option<Vec<Vec<Connection>>>,
    // Allow additional dynamic keys for SWITCH cases
    #[serde(flatten)]
    pub other: HashMap<String, Vec<Vec<Connection>>>,
}

/// Workflow metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "viewMode")]
    pub view_mode: Option<String>,
    #[serde(flatten)]
    pub other: HashMap<String, serde_json::Value>,
}

/// State definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "stateImages")]
    pub state_images: Vec<StateImage>,
    #[serde(default)]
    pub regions: Vec<StateRegion>,
    #[serde(default)]
    pub locations: Vec<StateLocation>,
    #[serde(default)]
    pub strings: Vec<StateString>,
    pub position: Position,
    #[serde(skip_serializing_if = "Option::is_none", rename = "entryActions")]
    pub entry_actions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "exitActions")]
    pub exit_actions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "isInitial")]
    pub is_initial: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "isFinal")]
    pub is_final: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "screenAssociation")]
    pub screen_association: Option<i32>,
    /// Rich description for AI verification agent
    #[serde(skip_serializing_if = "Option::is_none", rename = "aiDescription")]
    pub ai_description: Option<StateDescription>,
}

/// Position coordinates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// State image with patterns
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateImage {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub patterns: Vec<Pattern>,
    #[serde(default)]
    pub shared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "searchRegions")]
    pub search_regions: Option<Vec<SearchRegion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitors: Option<Vec<u32>>,
}

/// Pattern definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "imageId")]
    pub image_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "searchRegions")]
    pub search_regions: Option<Vec<SearchRegion>>,
    #[serde(default)]
    pub fixed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "targetPosition")]
    pub target_position: Option<TargetPosition>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "offsetX")]
    pub offset_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "offsetY")]
    pub offset_y: Option<i32>,
}

/// Target position within pattern
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetPosition {
    #[serde(rename = "percentW")]
    pub percent_w: f64,
    #[serde(rename = "percentH")]
    pub percent_h: f64,
}

/// Search region
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRegion {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none", rename = "referenceImageId")]
    pub reference_image_id: Option<String>,
}

/// State region
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRegion {
    pub id: String,
    pub name: String,
    pub bounds: Region,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "isSearchRegion")]
    pub is_search_region: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "isInteractionRegion"
    )]
    pub is_interaction_region: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitors: Option<Vec<u32>>,
}

/// Region bounds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// State location
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateLocation {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitors: Option<Vec<u32>>,
}

/// State string
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateString {
    pub id: String,
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "inputText")]
    pub input_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "expectedText")]
    pub expected_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex: Option<bool>,
}

/// Transition between states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    #[serde(rename = "type")]
    pub transition_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub workflows: Vec<String>,
    pub timeout: f64,
    #[serde(rename = "retryCount")]
    pub retry_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    // OutgoingTransition fields
    #[serde(skip_serializing_if = "Option::is_none", rename = "fromState")]
    pub from_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "toState")]
    pub to_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "staysVisible")]
    pub stays_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "activateStates")]
    pub activate_states: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "deactivateStates")]
    pub deactivate_states: Option<Vec<String>>,
    // IncomingTransition fields
    #[serde(skip_serializing_if = "Option::is_none", rename = "executeAfter")]
    pub execute_after: Option<Vec<String>>,
    /// Rich description for AI verification agent
    #[serde(skip_serializing_if = "Option::is_none", rename = "aiDescription")]
    pub ai_description: Option<TransitionDescription>,
}

/// Configuration settings
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recognition: Option<RecognitionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<PerformanceSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub find: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<serde_json::Value>,
}

/// Execution settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSettings {
    #[serde(rename = "defaultTimeout")]
    pub default_timeout: f64,
    #[serde(rename = "defaultRetryCount")]
    pub default_retry_count: u32,
    #[serde(rename = "actionDelay")]
    pub action_delay: f64,
    #[serde(rename = "failureStrategy")]
    pub failure_strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headless: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// Resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// Recognition settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecognitionSettings {
    #[serde(rename = "defaultThreshold")]
    pub default_threshold: f64,
    #[serde(rename = "searchAlgorithm")]
    pub search_algorithm: String,
    #[serde(rename = "multiScaleSearch")]
    pub multi_scale_search: bool,
    #[serde(rename = "colorSpace")]
    pub color_space: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "edgeDetection")]
    pub edge_detection: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "ocrEnabled")]
    pub ocr_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "ocrLanguage")]
    pub ocr_language: Option<String>,
}

/// Logging settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingSettings {
    pub level: String,
    #[serde(rename = "screenshotOnError")]
    pub screenshot_on_error: bool,
    #[serde(skip_serializing_if = "Option::is_none", rename = "logFile")]
    pub log_file: Option<String>,
    #[serde(rename = "consoleOutput")]
    pub console_output: bool,
    #[serde(rename = "detailedMatching")]
    pub detailed_matching: bool,
}

/// Performance settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSettings {
    #[serde(rename = "maxParallelActions")]
    pub max_parallel_actions: u32,
    #[serde(skip_serializing_if = "Option::is_none", rename = "cpuLimit")]
    pub cpu_limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "memoryLimit")]
    pub memory_limit: Option<u64>,
    #[serde(rename = "cacheImages")]
    pub cache_images: bool,
    #[serde(rename = "optimizeSearch")]
    pub optimize_search: bool,
}

/// Schedule definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "triggerType")]
    pub trigger_type: String,
    #[serde(rename = "checkMode")]
    pub check_mode: String,
    #[serde(rename = "scheduleType")]
    pub schedule_type: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "cronExpression")]
    pub cron_expression: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "intervalSeconds")]
    pub interval_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "triggerState")]
    pub trigger_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxIterations")]
    pub max_iterations: Option<u32>,
    #[serde(rename = "stateCheckDelaySeconds")]
    pub state_check_delay_seconds: f64,
    #[serde(rename = "stateRebuildDelaySeconds")]
    pub state_rebuild_delay_seconds: f64,
    #[serde(rename = "failureThreshold")]
    pub failure_threshold: u32,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal_config() {
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
            "categories": []
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.version, "2.0.0");
        assert_eq!(config.metadata.name, "Test Project");
        assert_eq!(config.project_id(), "test-project");
    }

    #[test]
    fn test_deserialize_with_state_images() {
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "images": [
                {"id": "img1", "name": "test.png", "data": "base64...", "format": "png", "width": 100, "height": 100}
            ],
            "workflows": [],
            "states": [
                {
                    "id": "state1",
                    "name": "State 1",
                    "stateImages": [
                        {
                            "id": "si1",
                            "name": "State Image 1",
                            "patterns": [
                                {"id": "p1", "imageId": "img1", "fixed": false}
                            ],
                            "shared": false
                        }
                    ],
                    "regions": [],
                    "locations": [],
                    "strings": [],
                    "position": {"x": 0, "y": 0}
                }
            ],
            "transitions": [],
            "categories": []
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.state_image_count(), 1);
        assert_eq!(config.pattern_count(), 1);
    }

    #[test]
    fn test_deserialize_categories() {
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "categories": [
                {"name": "Main", "automationEnabled": true},
                {"name": "Incoming Transitions", "automationEnabled": false},
                {"name": "Outgoing Transitions", "automationEnabled": false}
            ]
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.categories.len(), 3);
        assert_eq!(config.categories[0].name, "Main");
        assert!(config.categories[0].automation_enabled);
        assert_eq!(config.categories[1].name, "Incoming Transitions");
        assert!(!config.categories[1].automation_enabled);
        assert_eq!(config.categories[2].name, "Outgoing Transitions");
        assert!(!config.categories[2].automation_enabled);
    }

    #[test]
    fn test_deserialize_with_ai_descriptions() {
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test AI Descriptions",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "images": [],
            "workflows": [
                {
                    "id": "workflow1",
                    "name": "Login Workflow",
                    "format": "graph",
                    "version": "1.0",
                    "actions": [],
                    "connections": {},
                    "aiDescription": {
                        "purpose": "Complete user login process",
                        "success_criteria": "User sees dashboard after login"
                    }
                }
            ],
            "states": [
                {
                    "id": "state1",
                    "name": "Login Page",
                    "stateImages": [],
                    "regions": [],
                    "locations": [],
                    "strings": [],
                    "position": {"x": 0, "y": 0},
                    "aiDescription": {
                        "summary": "Login page with username and password fields",
                        "expected_elements": ["Username field", "Password field", "Login button"],
                        "unexpected_elements": ["Error dialog"],
                        "user_goal": "User wants to authenticate"
                    }
                }
            ],
            "transitions": [
                {
                    "id": "trans1",
                    "type": "outgoing",
                    "fromState": "state1",
                    "toState": "state2",
                    "workflows": [],
                    "timeout": 30.0,
                    "retryCount": 3,
                    "aiDescription": {
                        "intent": "Submit login credentials",
                        "preconditions": ["Username and password entered"],
                        "postconditions": ["Dashboard is visible"],
                        "expected_duration_ms": 2000
                    }
                }
            ],
            "categories": []
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.version, "2.0.0");

        // Check workflow AI description
        let workflow = &config.workflows[0];
        assert!(workflow.ai_description.is_some());
        let workflow_desc = workflow.ai_description.as_ref().unwrap();
        assert_eq!(workflow_desc.purpose, "Complete user login process");
        assert_eq!(
            workflow_desc.success_criteria,
            "User sees dashboard after login"
        );

        // Check state AI description
        let state = &config.states[0];
        assert!(state.ai_description.is_some());
        let state_desc = state.ai_description.as_ref().unwrap();
        assert_eq!(
            state_desc.summary,
            "Login page with username and password fields"
        );
        assert_eq!(
            state_desc.expected_elements,
            Some(vec![
                "Username field".to_string(),
                "Password field".to_string(),
                "Login button".to_string()
            ])
        );
        assert_eq!(
            state_desc.unexpected_elements,
            Some(vec!["Error dialog".to_string()])
        );
        assert_eq!(
            state_desc.user_goal,
            Some("User wants to authenticate".to_string())
        );

        // Check transition AI description
        let transition = &config.transitions[0];
        assert!(transition.ai_description.is_some());
        let trans_desc = transition.ai_description.as_ref().unwrap();
        assert_eq!(trans_desc.intent, "Submit login credentials");
        assert_eq!(
            trans_desc.preconditions,
            Some(vec!["Username and password entered".to_string()])
        );
        assert_eq!(
            trans_desc.postconditions,
            Some(vec!["Dashboard is visible".to_string()])
        );
        assert_eq!(trans_desc.expected_duration_ms, Some(2000));
    }

    #[test]
    fn test_deserialize_without_ai_descriptions_backward_compatible() {
        // Test that configs without AI descriptions still work (backward compatibility)
        let json = r#"{
            "version": "2.0.0",
            "metadata": {
                "name": "Test Backward Compat",
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z"
            },
            "images": [],
            "workflows": [
                {
                    "id": "workflow1",
                    "name": "Simple Workflow",
                    "format": "graph",
                    "version": "1.0",
                    "actions": [],
                    "connections": {}
                }
            ],
            "states": [
                {
                    "id": "state1",
                    "name": "Simple State",
                    "stateImages": [],
                    "regions": [],
                    "locations": [],
                    "strings": [],
                    "position": {"x": 0, "y": 0}
                }
            ],
            "transitions": [
                {
                    "id": "trans1",
                    "type": "outgoing",
                    "fromState": "state1",
                    "toState": "state2",
                    "workflows": [],
                    "timeout": 30.0,
                    "retryCount": 3
                }
            ],
            "categories": []
        }"#;

        let config: QontinuiConfig = serde_json::from_str(json).unwrap();

        // Verify all AI descriptions are None (backward compatible)
        assert!(config.workflows[0].ai_description.is_none());
        assert!(config.states[0].ai_description.is_none());
        assert!(config.transitions[0].ai_description.is_none());
    }
}
