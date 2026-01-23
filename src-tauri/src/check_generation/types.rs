//! Types for check generation module
//!
//! Contains all request/response types for workspace scanning and AI check generation.

use serde::{Deserialize, Serialize};

/// Request to scan a workspace for projects
#[derive(Debug, Deserialize)]
pub struct ScanWorkspaceRequest {
    /// Base directory to start scanning from
    pub base_directory: String,
    /// Maximum depth to scan (1 = immediate subdirectories only)
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

fn default_max_depth() -> u32 {
    2
}

/// Result of scanning a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceScanResult {
    /// List of detected projects
    pub projects: Vec<DetectedProject>,
    /// Total directories scanned
    pub total_directories_scanned: u32,
    /// Base directory that was scanned
    pub base_directory: String,
}

/// A single detected project in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedProject {
    /// Absolute path to the project
    pub path: String,
    /// Path relative to the scan base directory
    pub relative_path: String,
    /// Detected programming languages
    pub languages: Vec<String>,
    /// Config files found in the project
    pub config_files: Vec<String>,
    /// Project name (usually directory name)
    pub name: String,
}

/// Request to generate check suggestions
#[derive(Debug, Deserialize)]
pub struct GenerateChecksRequest {
    /// Results from workspace scan
    pub workspace_scan: WorkspaceScanResult,
    /// Optional user preferences
    #[serde(default)]
    pub user_preferences: Option<UserPreferences>,
}

/// User preferences for check generation
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserPreferences {
    /// Prefer auto-fix when available
    #[serde(default)]
    pub prefer_auto_fix: bool,
    /// Include security checks
    #[serde(default = "default_true")]
    pub include_security: bool,
    /// Include formatting checks
    #[serde(default = "default_true")]
    pub include_formatting: bool,
    /// Include type checking
    #[serde(default = "default_true")]
    pub include_type_checking: bool,
}

fn default_true() -> bool {
    true
}

/// A suggested check from AI generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedCheckWithReason {
    /// The check configuration
    pub check: CreateCheckInput,
    /// Reason why this check is suggested
    pub reason: String,
    /// Path to the project this check applies to
    pub project_path: String,
}

/// Result of generating checks
#[derive(Debug, Serialize)]
pub struct GenerateChecksResponse {
    /// List of suggested checks
    pub suggested_checks: Vec<SuggestedCheckWithReason>,
    /// Whether generation was successful
    pub success: bool,
    /// Optional error message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Input for creating a new check (matches frontend CreateCheckInput)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCheckInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub check_type: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(default)]
    pub auto_fix: bool,
    #[serde(default)]
    pub fail_on_warning: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub is_critical: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub ai_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_generation_prompt: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_timeout() -> u32 {
    60
}

impl Default for CreateCheckInput {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            check_type: "lint".to_string(),
            tool: "custom".to_string(),
            command: None,
            working_directory: None,
            config_path: None,
            auto_fix: false,
            fail_on_warning: false,
            timeout_seconds: 60,
            is_critical: false,
            enabled: true,
            ai_generated: true,
            ai_generation_prompt: None,
            tags: Vec::new(),
        }
    }
}
