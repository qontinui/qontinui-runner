//! AWAS (Application Web Automation Specification) Step Types
//!
//! Defines step types for AWAS automation:
//! - `awas_discover` - Discover AWAS manifest for a URL
//! - `awas_execute` - Execute an AWAS action
//! - `awas_check_support` - Check if URL supports AWAS
//! - `awas_list_actions` - List available actions from loaded manifest
//! - `awas_extract_elements` - Extract AWAS elements from current page HTML
//!
//! These steps use the Python bridge AWAS commands already implemented.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ============================================================================
// AWAS Step Types
// ============================================================================

/// AWAS Discover step configuration
///
/// Discovers the AWAS manifest from a given URL. This is typically the first
/// step when working with an AWAS-enabled application.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwasDiscoverStepConfig {
    /// URL to discover AWAS manifest from
    pub url: String,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// AWAS Execute step configuration
///
/// Executes an AWAS action on the target application. The action must be
/// defined in the application's AWAS manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwasExecuteStepConfig {
    /// URL of the application
    pub url: String,
    /// Action ID to execute (from the AWAS manifest)
    pub action_id: String,
    /// Optional parameters for the action
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// AWAS Check Support step configuration
///
/// Checks if a URL supports AWAS. Returns information about AWAS availability
/// including version and manifest URL.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwasCheckSupportStepConfig {
    /// URL to check for AWAS support
    pub url: String,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// AWAS List Actions step configuration
///
/// Lists available AWAS actions from a previously discovered manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwasListActionsStepConfig {
    /// URL to list actions for (optional, uses last discovered manifest if not provided)
    #[serde(default)]
    pub url: Option<String>,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// AWAS Extract Elements step configuration
///
/// Extracts AWAS-annotated elements from HTML content. Useful for analyzing
/// page structure and finding available automation targets.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwasExtractElementsStepConfig {
    /// HTML content to extract elements from
    pub html: String,
    /// Optional base URL for resolving relative URLs
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Default timeout for AWAS operations.
/// Returns 0 to indicate no timeout (run until completion).
fn default_timeout() -> u64 {
    0 // No timeout - run until completion
}

// ============================================================================
// AWAS Step Results
// ============================================================================

/// Result from AWAS Discover step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasDiscoverResult {
    /// Whether discovery was successful
    pub success: bool,
    /// The discovered AWAS manifest (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result from AWAS Execute step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasExecuteResult {
    /// Whether execution was successful
    pub success: bool,
    /// Result data from the action (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result from AWAS Check Support step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasCheckSupportResult {
    /// Whether the URL supports AWAS
    pub supported: bool,
    /// AWAS version (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Manifest URL (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    /// Error message (if check failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Single AWAS action info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasActionInfo {
    /// Action identifier
    pub id: String,
    /// Human-readable action name
    pub name: String,
    /// Description of what the action does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Required parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Result from AWAS List Actions step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasListActionsResult {
    /// Whether listing was successful
    pub success: bool,
    /// List of available actions
    pub actions: Vec<AwasActionInfo>,
    /// URL the actions are from
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result from AWAS Extract Elements step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwasExtractElementsResult {
    /// Whether extraction was successful
    pub success: bool,
    /// Extracted elements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ============================================================================
// AWAS Step Type Enum
// ============================================================================

/// AWAS step types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum AwasStepType {
    /// Discover AWAS manifest for a URL
    AwasDiscover,
    /// Execute an AWAS action
    AwasExecute,
    /// Check if URL supports AWAS
    AwasCheckSupport,
    /// List available AWAS actions
    AwasListActions,
    /// Extract AWAS elements from HTML
    AwasExtractElements,
}

impl AwasStepType {
    /// Returns the string representation of the step type
    pub fn as_str(&self) -> &'static str {
        match self {
            AwasStepType::AwasDiscover => "awas_discover",
            AwasStepType::AwasExecute => "awas_execute",
            AwasStepType::AwasCheckSupport => "awas_check_support",
            AwasStepType::AwasListActions => "awas_list_actions",
            AwasStepType::AwasExtractElements => "awas_extract_elements",
        }
    }

    /// Returns the display name for the step type
    pub fn display_name(&self) -> &'static str {
        match self {
            AwasStepType::AwasDiscover => "AWAS Discover",
            AwasStepType::AwasExecute => "AWAS Execute",
            AwasStepType::AwasCheckSupport => "AWAS Check Support",
            AwasStepType::AwasListActions => "AWAS List Actions",
            AwasStepType::AwasExtractElements => "AWAS Extract Elements",
        }
    }

    /// Returns a description of what the step does
    pub fn description(&self) -> &'static str {
        match self {
            AwasStepType::AwasDiscover => "Discover AWAS manifest from a URL",
            AwasStepType::AwasExecute => "Execute an AWAS action on the target application",
            AwasStepType::AwasCheckSupport => "Check if a URL supports AWAS",
            AwasStepType::AwasListActions => "List available AWAS actions from the manifest",
            AwasStepType::AwasExtractElements => "Extract AWAS elements from HTML content",
        }
    }
}

impl std::fmt::Display for AwasStepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for AwasStepType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "awas_discover" => Ok(AwasStepType::AwasDiscover),
            "awas_execute" => Ok(AwasStepType::AwasExecute),
            "awas_check_support" => Ok(AwasStepType::AwasCheckSupport),
            "awas_list_actions" => Ok(AwasStepType::AwasListActions),
            "awas_extract_elements" => Ok(AwasStepType::AwasExtractElements),
            _ => Err(format!("Unknown AWAS step type: {}", s)),
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a step type string is an AWAS step
pub fn is_awas_step_type(step_type: &str) -> bool {
    matches!(
        step_type,
        "awas_discover"
            | "awas_execute"
            | "awas_check_support"
            | "awas_list_actions"
            | "awas_extract_elements"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_step_type_from_str() {
        assert_eq!(
            "awas_discover".parse::<AwasStepType>().unwrap(),
            AwasStepType::AwasDiscover
        );
        assert_eq!(
            "awas_execute".parse::<AwasStepType>().unwrap(),
            AwasStepType::AwasExecute
        );
        assert!("unknown".parse::<AwasStepType>().is_err());
    }

    #[test]
    fn test_is_awas_step_type() {
        assert!(is_awas_step_type("awas_discover"));
        assert!(is_awas_step_type("awas_execute"));
        assert!(is_awas_step_type("awas_check_support"));
        assert!(is_awas_step_type("awas_list_actions"));
        assert!(is_awas_step_type("awas_extract_elements"));
        assert!(!is_awas_step_type("workflow"));
        assert!(!is_awas_step_type("playwright"));
    }
}
