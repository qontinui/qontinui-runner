//! Unified Workflows
//!
//! This module provides types for the unified Workflow Builder system.
//! All workflows are organized into three phases: Setup, Verification, Agentic.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Log source selection mode for a workflow
/// - "default": Use the global default profile (from Settings)
/// - "ai": Let AI automatically select relevant sources
/// - "all": Use all enabled log sources
/// - { "profile_id": "..." }: Use a specific profile
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LogSourceSelection {
    /// Simple string modes: "default", "ai", "all"
    Mode(String),
    /// Specific profile selection
    Profile { profile_id: String },
}

impl Default for LogSourceSelection {
    fn default() -> Self {
        LogSourceSelection::Mode("default".to_string())
    }
}

/// A unified workflow with steps organized by phase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedWorkflow {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name
    pub name: String,
    /// Description of what this workflow does
    #[serde(default)]
    pub description: String,
    /// Category for organization
    #[serde(default = "default_category")]
    pub category: String,
    /// Tags for filtering
    #[serde(default)]
    pub tags: Vec<String>,

    /// Setup phase steps (JSON array)
    #[serde(default)]
    pub setup_steps: Vec<Value>,
    /// Verification phase steps (JSON array)
    #[serde(default)]
    pub verification_steps: Vec<Value>,
    /// Agentic phase steps (JSON array)
    #[serde(default)]
    pub agentic_steps: Vec<Value>,
    /// Completion phase steps (JSON array) - runs once after the verification loop exits
    #[serde(default)]
    pub completion_steps: Vec<Value>,

    /// Maximum iterations for agentic phase
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// AI provider override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Skip AI summary generation at the end (default: false, meaning AI summary is generated)
    #[serde(default)]
    pub skip_ai_summary: bool,

    /// Log source selection for this workflow
    /// - "default": Use the global default profile (from Settings → Log Sources)
    /// - "ai": Let AI automatically select relevant sources based on context
    /// - "all": Use all enabled log sources
    /// - { "profile_id": "..." }: Use a specific profile
    #[serde(default, skip_serializing_if = "is_default_log_source")]
    pub log_source_selection: LogSourceSelection,

    /// Manually added context IDs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_ids: Vec<String>,

    /// Disabled context IDs (excluded from auto-include)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_context_ids: Vec<String>,

    /// Whether to auto-include contexts based on task mentions (default: true)
    #[serde(default = "default_auto_include_contexts")]
    pub auto_include_contexts: bool,

    /// Custom developer prompt template for this workflow
    /// When set, this template is used instead of the global default when running the workflow.
    /// Supports variables: {{SESSION_ID}}, {{ITERATION}}, {{MAX_ITERATIONS}}, {{GOAL}},
    /// {{EXECUTION_STEPS}}, {{WORKSPACE_ESCAPED}}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_template: Option<String>,

    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification (serialized as "modified_at" to match frontend)
    #[serde(rename = "modified_at")]
    pub updated_at: String,
}

fn default_auto_include_contexts() -> bool {
    true
}

fn default_category() -> String {
    "general".to_string()
}

fn default_max_iterations() -> u32 {
    10
}

fn is_default_log_source(selection: &LogSourceSelection) -> bool {
    matches!(selection, LogSourceSelection::Mode(s) if s == "default")
}

/// Request body for creating a new unified workflow
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUnifiedWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub setup_steps: Vec<Value>,
    #[serde(default)]
    pub verification_steps: Vec<Value>,
    #[serde(default)]
    pub agentic_steps: Vec<Value>,
    #[serde(default)]
    pub completion_steps: Vec<Value>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub skip_ai_summary: bool,
    #[serde(default)]
    pub log_source_selection: LogSourceSelection,
    #[serde(default)]
    pub context_ids: Vec<String>,
    #[serde(default)]
    pub disabled_context_ids: Vec<String>,
    #[serde(default = "default_auto_include_contexts")]
    pub auto_include_contexts: bool,
    /// Custom developer prompt template for this workflow
    pub prompt_template: Option<String>,
}

/// Request body for updating a unified workflow
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUnifiedWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub setup_steps: Option<Vec<Value>>,
    pub verification_steps: Option<Vec<Value>>,
    pub agentic_steps: Option<Vec<Value>>,
    pub completion_steps: Option<Vec<Value>>,
    pub max_iterations: Option<u32>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub skip_ai_summary: Option<bool>,
    pub log_source_selection: Option<LogSourceSelection>,
    pub context_ids: Option<Vec<String>>,
    pub disabled_context_ids: Option<Vec<String>>,
    pub auto_include_contexts: Option<bool>,
    /// Custom developer prompt template for this workflow
    pub prompt_template: Option<String>,
}

/// Query parameters for searching unified workflows
#[derive(Debug, Clone, Deserialize)]
pub struct SearchUnifiedWorkflowsQuery {
    /// Search query (matches name, description)
    pub q: Option<String>,
    /// Filter by category
    pub category: Option<String>,
    /// Filter by tag
    pub tag: Option<String>,
}

/// Manifest for exported workflow files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowExportManifest {
    /// Export format version
    pub version: String,
    /// When the export was created
    pub exported_at: String,
    /// App version that created the export
    pub app_version: String,
    /// Type of content
    pub content_type: String,
}

/// A single workflow export file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowExport {
    /// Export manifest with version info
    pub manifest: WorkflowExportManifest,
    /// The workflow data
    pub workflow: UnifiedWorkflow,
}

/// Request body for importing a workflow
#[derive(Debug, Clone, Deserialize)]
pub struct ImportWorkflowRequest {
    /// The exported workflow data
    pub workflow: UnifiedWorkflow,
    /// How to handle ID conflicts: "keep" (use original ID), "generate" (new ID), "overwrite" (replace existing)
    #[serde(default = "default_conflict_strategy")]
    pub conflict_strategy: String,
}

fn default_conflict_strategy() -> String {
    "generate".to_string()
}

/// Result of importing a workflow
#[derive(Debug, Clone, Serialize)]
pub struct ImportWorkflowResult {
    /// The imported workflow
    pub workflow: UnifiedWorkflow,
    /// Whether an existing workflow was overwritten
    pub overwritten: bool,
    /// Original ID if it was changed
    pub original_id: Option<String>,
}
