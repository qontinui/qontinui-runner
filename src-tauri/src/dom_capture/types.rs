//! DOM Capture types and data structures.

use serde::{Deserialize, Serialize};

/// Source of the DOM capture
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DomCaptureSource {
    /// Captured via Playwright CDP connection
    Playwright,
    /// Captured via browser extension
    Extension,
}

impl std::fmt::Display for DomCaptureSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomCaptureSource::Playwright => write!(f, "playwright"),
            DomCaptureSource::Extension => write!(f, "extension"),
        }
    }
}

impl std::str::FromStr for DomCaptureSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "playwright" => Ok(DomCaptureSource::Playwright),
            "extension" => Ok(DomCaptureSource::Extension),
            _ => Err(format!("Unknown DOM capture source: {}", s)),
        }
    }
}

/// What triggered the DOM capture
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DomCaptureTrigger {
    /// Captured on-demand via MCP tool or API
    OnDemand,
    /// Automatically captured (e.g., during workflow)
    Auto,
    /// Captured due to an error (e.g., test failure)
    Error,
}

impl std::fmt::Display for DomCaptureTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomCaptureTrigger::OnDemand => write!(f, "on_demand"),
            DomCaptureTrigger::Auto => write!(f, "auto"),
            DomCaptureTrigger::Error => write!(f, "error"),
        }
    }
}

impl std::str::FromStr for DomCaptureTrigger {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "on_demand" | "ondemand" => Ok(DomCaptureTrigger::OnDemand),
            "auto" => Ok(DomCaptureTrigger::Auto),
            "error" => Ok(DomCaptureTrigger::Error),
            _ => Err(format!("Unknown DOM capture trigger: {}", s)),
        }
    }
}

/// Type of DOM capture
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DomCaptureType {
    /// Full page HTML (document.documentElement.outerHTML)
    Full,
    /// Partial capture via CSS selector
    Selector,
}

impl std::fmt::Display for DomCaptureType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomCaptureType::Full => write!(f, "full"),
            DomCaptureType::Selector => write!(f, "selector"),
        }
    }
}

/// A DOM capture record (returned via API)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomCapture {
    /// Unique identifier
    pub id: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Page URL
    pub url: String,
    /// Document title
    pub page_title: String,
    /// Type of capture (full or selector)
    pub capture_type: DomCaptureType,
    /// CSS selector if partial capture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Size of HTML content in bytes
    pub html_size_bytes: usize,
    /// Path to the HTML file
    pub html_file_path: String,
    /// Path to related screenshot (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_screenshot: Option<String>,
    /// Source of the capture
    pub source: DomCaptureSource,
    /// Associated task run ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// What triggered the capture
    pub trigger: DomCaptureTrigger,
}

/// DOM capture file entry for JSONL storage
/// References the HTML file instead of including content inline
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomCaptureFileEntry {
    /// Unique identifier
    pub id: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Page URL
    pub url: String,
    /// Document title
    pub page_title: String,
    /// Type of capture (full or selector)
    pub capture_type: String,
    /// CSS selector if partial capture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Size of HTML content in bytes
    pub html_size_bytes: usize,
    /// Path to the HTML file (relative to .dev-logs)
    pub html_file_path: String,
    /// Path to related screenshot (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_screenshot: Option<String>,
    /// Source of the capture
    pub source: String,
    /// Associated task run ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// What triggered the capture
    pub trigger: String,
}

impl From<DomCapture> for DomCaptureFileEntry {
    fn from(capture: DomCapture) -> Self {
        DomCaptureFileEntry {
            id: capture.id,
            timestamp: capture.timestamp,
            url: capture.url,
            page_title: capture.page_title,
            capture_type: capture.capture_type.to_string(),
            selector: capture.selector,
            html_size_bytes: capture.html_size_bytes,
            html_file_path: capture.html_file_path,
            linked_screenshot: capture.linked_screenshot,
            source: capture.source.to_string(),
            task_run_id: capture.task_run_id,
            trigger: capture.trigger.to_string(),
        }
    }
}

impl TryFrom<DomCaptureFileEntry> for DomCapture {
    type Error = String;

    fn try_from(entry: DomCaptureFileEntry) -> Result<Self, Self::Error> {
        Ok(DomCapture {
            id: entry.id,
            timestamp: entry.timestamp,
            url: entry.url,
            page_title: entry.page_title,
            capture_type: if entry.capture_type == "selector" {
                DomCaptureType::Selector
            } else {
                DomCaptureType::Full
            },
            selector: entry.selector,
            html_size_bytes: entry.html_size_bytes,
            html_file_path: entry.html_file_path,
            linked_screenshot: entry.linked_screenshot,
            source: entry.source.parse()?,
            task_run_id: entry.task_run_id,
            trigger: entry.trigger.parse()?,
        })
    }
}

/// Request to receive DOM from browser extension
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveExtensionDomRequest {
    /// Page URL
    pub url: String,
    /// Document title
    pub page_title: String,
    /// Full HTML content
    pub html: String,
    /// CSS selector if partial capture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Task run ID to associate with
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
}

/// Request to capture DOM
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDomRequest {
    /// Target URL (uses current if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// CSS selector for partial capture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Capture source: "playwright" or "extension"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// What triggered the capture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Task run ID to associate with
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// Path to screenshot to link
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_screenshot: Option<String>,
}
