//! Types for the Recording & Playback system

use serde::{Deserialize, Serialize};

/// Recording status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RecordingStatus {
    #[default]
    Recording,
    Completed,
    Failed,
    Cancelled,
}


impl std::fmt::Display for RecordingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recording => write!(f, "recording"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for RecordingStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "recording" => Ok(Self::Recording),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown recording status: {}", s)),
        }
    }
}

/// Action types that can be recorded
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Click,
    Type,
    Navigate,
    Select,
    Scroll,
    Hover,
    Keypress,
}

impl std::fmt::Display for ActionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Click => write!(f, "click"),
            Self::Type => write!(f, "type"),
            Self::Navigate => write!(f, "navigate"),
            Self::Select => write!(f, "select"),
            Self::Scroll => write!(f, "scroll"),
            Self::Hover => write!(f, "hover"),
            Self::Keypress => write!(f, "keypress"),
        }
    }
}

impl std::str::FromStr for ActionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "click" => Ok(Self::Click),
            "type" => Ok(Self::Type),
            "navigate" => Ok(Self::Navigate),
            "select" => Ok(Self::Select),
            "scroll" => Ok(Self::Scroll),
            "hover" => Ok(Self::Hover),
            "keypress" => Ok(Self::Keypress),
            _ => Err(format!("Unknown action type: {}", s)),
        }
    }
}

/// Export format for generated scripts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Python,
    Playwright,
    Pytest,
    Cypress,
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Python => write!(f, "python"),
            Self::Playwright => write!(f, "playwright"),
            Self::Pytest => write!(f, "pytest"),
            Self::Cypress => write!(f, "cypress"),
        }
    }
}

impl std::str::FromStr for ExportFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "python" => Ok(Self::Python),
            "playwright" => Ok(Self::Playwright),
            "pytest" => Ok(Self::Pytest),
            "cypress" => Ok(Self::Cypress),
            _ => Err(format!("Unknown export format: {}", s)),
        }
    }
}

/// A recording session containing multiple actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub base_url: String,
    pub action_count: i32,
    pub status: RecordingStatus,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_info: Option<BrowserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i32>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Browser information for a recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// Target element information for an action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTarget {
    /// UI Bridge element ID (from bridge registry or data-testid attribute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_id: Option<String>,
    /// HTML tag name
    pub tag_name: String,
    /// Generated CSS selector
    pub selector: String,
    /// XPath selector (fallback)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpath: Option<String>,
    /// Visible text content (truncated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    /// Bounding box
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BoundingBox>,
    /// Additional attributes for identification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<ElementAttributes>,
}

/// Bounding box for element positioning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Element attributes for better identification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementAttributes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
}

/// Click-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickData {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub button: MouseButton,
    #[serde(default = "default_click_count")]
    pub click_count: i32,
}

fn default_click_count() -> i32 {
    1
}

/// Mouse button for clicks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Type-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeData {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// Whether to clear existing value first
    #[serde(default)]
    pub clear_first: bool,
}

/// Navigate-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigateData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_url: Option<String>,
    pub to_url: String,
    #[serde(default)]
    pub navigation_type: NavigationType,
}

/// Navigation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NavigationType {
    #[default]
    Click,
    Enter,
    UrlChange,
    Redirect,
    PopState,
    HashChange,
}

/// Select-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectData {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_index: Option<i32>,
}

/// Scroll-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollData {
    pub delta_x: f64,
    pub delta_y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_left: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_top: Option<f64>,
}

/// Keypress-specific action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeypressData {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default)]
    pub ctrl_key: bool,
    #[serde(default)]
    pub shift_key: bool,
    #[serde(default)]
    pub alt_key: bool,
    #[serde(default)]
    pub meta_key: bool,
}

/// Action-specific data (discriminated union)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionData {
    Click(ClickData),
    Type(TypeData),
    Navigate(NavigateData),
    Select(SelectData),
    Scroll(ScrollData),
    Keypress(KeypressData),
}

/// A recorded user action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedAction {
    pub id: String,
    pub recording_id: String,
    pub sequence_number: i32,
    pub action_type: ActionType,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    pub target: ActionTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_data: Option<ActionData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

/// Input for creating a new recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRecordingInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_info: Option<BrowserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i32>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Input for adding an action to a recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddActionInput {
    pub action_type: ActionType,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    pub target: ActionTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_data: Option<serde_json::Value>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// Options for exporting a recording
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExportOptions {
    /// Wait strategy: 'networkidle', 'domcontentloaded', 'load', 'fixed'
    #[serde(default = "default_wait_strategy")]
    pub wait_strategy: String,
    /// Fixed wait time in milliseconds (used when wait_strategy is 'fixed')
    #[serde(default = "default_fixed_wait_ms")]
    pub fixed_wait_ms: i64,
    /// Selector priority: 'ui_id', 'css', 'xpath', 'text'
    #[serde(default = "default_selector_priority")]
    pub selector_priority: Vec<String>,
    /// Include assertions for element visibility
    #[serde(default)]
    pub include_visibility_assertions: bool,
    /// Include timing assertions
    #[serde(default)]
    pub include_timing_assertions: bool,
    /// Test name (for pytest/playwright)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_name: Option<String>,
    /// Test description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_description: Option<String>,
}

fn default_wait_strategy() -> String {
    "networkidle".to_string()
}

fn default_fixed_wait_ms() -> i64 {
    1000
}

fn default_selector_priority() -> Vec<String> {
    vec!["ui_id".to_string(), "css".to_string(), "xpath".to_string()]
}

/// A recording export record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingExport {
    pub id: String,
    pub recording_id: String,
    pub export_format: ExportFormat,
    pub script_content: String,
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<ExportOptions>,
    pub created_at: String,
}

/// Snapshot from the browser extension during recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSnapshot {
    pub id: String,
    pub timestamp: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub trigger: String, // 'click', 'input', 'navigation', 'initial', 'mutation', 'manual'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_element: Option<TriggerElement>,
    #[serde(default)]
    pub elements: Vec<SnapshotElement>,
    #[serde(default)]
    pub element_count: i32,
    /// Enhanced action capture data (new in v2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<CapturedAction>,
}

/// Trigger element info from snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerElement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
}

/// Element in a snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotElement {
    pub id: String,
    pub tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<ElementAttributes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BoundingBox>,
    #[serde(default)]
    pub is_visible: bool,
    #[serde(default)]
    pub is_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Captured action with full detail from enhanced recorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedAction {
    pub id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub url: String,
    pub target: ActionTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click: Option<ClickData>,
    #[serde(rename = "type_data", skip_serializing_if = "Option::is_none")]
    pub type_data: Option<TypeData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate: Option<NavigateData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<SelectData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<ScrollData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keypress: Option<KeypressData>,
}
