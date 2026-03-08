//! Known issue types for persistent, cross-run issue tracking.
//!
//! These types define the data model for issues that survive across
//! workflow runs, enabling issue-aware verification.

use serde::{Deserialize, Serialize};

/// Category of a known issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    Duplication,
    Rendering,
    DataIntegrity,
    Timing,
    Layout,
    State,
    Performance,
    Encoding,
    Navigation,
    Authentication,
    Other,
}

impl IssueCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Duplication => "duplication",
            Self::Rendering => "rendering",
            Self::DataIntegrity => "data_integrity",
            Self::Timing => "timing",
            Self::Layout => "layout",
            Self::State => "state",
            Self::Performance => "performance",
            Self::Encoding => "encoding",
            Self::Navigation => "navigation",
            Self::Authentication => "authentication",
            Self::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "duplication" => Some(Self::Duplication),
            "rendering" => Some(Self::Rendering),
            "data_integrity" => Some(Self::DataIntegrity),
            "timing" => Some(Self::Timing),
            "layout" => Some(Self::Layout),
            "state" => Some(Self::State),
            "performance" => Some(Self::Performance),
            "encoding" => Some(Self::Encoding),
            "navigation" => Some(Self::Navigation),
            "authentication" => Some(Self::Authentication),
            _ => Some(Self::Other),
        }
    }
}

/// How this issue is scoped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeType {
    Global,
    Spec,
    Url,
    Component,
    Feature,
}

impl ScopeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Spec => "spec",
            Self::Url => "url",
            Self::Component => "component",
            Self::Feature => "feature",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "spec" => Some(Self::Spec),
            "url" => Some(Self::Url),
            "component" => Some(Self::Component),
            "feature" => Some(Self::Feature),
            _ => None,
        }
    }
}

/// How an issue should be detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMethod {
    Algorithmic,
    AiJudgment,
    Visual,
    Command,
    UiBridge,
}

impl DetectionMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Algorithmic => "algorithmic",
            Self::AiJudgment => "ai_judgment",
            Self::Visual => "visual",
            Self::Command => "command",
            Self::UiBridge => "ui_bridge",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "algorithmic" => Some(Self::Algorithmic),
            "ai_judgment" => Some(Self::AiJudgment),
            "visual" => Some(Self::Visual),
            "command" => Some(Self::Command),
            "ui_bridge" => Some(Self::UiBridge),
            _ => None,
        }
    }
}

/// Severity of a known issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl IssueSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }

    /// Returns a numeric ordering value (lower = more severe).
    /// Useful for sorting and filtering by minimum severity.
    pub fn ordinal(&self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }
}

/// Lifecycle status of a known issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Active,
    Resolved,
    Monitoring,
    WontFix,
}

impl IssueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Resolved => "resolved",
            Self::Monitoring => "monitoring",
            Self::WontFix => "wont_fix",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "resolved" => Some(Self::Resolved),
            "monitoring" => Some(Self::Monitoring),
            "wont_fix" => Some(Self::WontFix),
            _ => None,
        }
    }
}

/// How this issue was created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueProvenance {
    Manual,
    AutoDetected,
    Reflection,
    Imported,
}

impl IssueProvenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AutoDetected => "auto_detected",
            Self::Reflection => "reflection",
            Self::Imported => "imported",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(Self::Manual),
            "auto_detected" => Some(Self::AutoDetected),
            "reflection" => Some(Self::Reflection),
            "imported" => Some(Self::Imported),
            _ => None,
        }
    }
}

/// A persistent known issue that survives across workflow runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownIssue {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: IssueCategory,
    pub scope_type: ScopeType,
    pub scope_value: Option<String>,
    pub scope_tags: Vec<String>,
    pub detection_method: DetectionMethod,
    pub detection_config: serde_json::Value,
    pub pattern_template_id: Option<String>,
    pub reproduction_context: Option<String>,
    pub trigger_conditions: Vec<String>,
    pub severity: IssueSeverity,
    pub status: IssueStatus,
    pub confidence: f64,
    pub provenance: IssueProvenance,
    pub source_finding_ids: Vec<String>,
    pub source_task_run_id: Option<String>,
    pub verification_hint: Option<String>,
    pub verification_step_template: Option<serde_json::Value>,
    pub times_detected: u32,
    pub times_checked: u32,
    pub last_detected_at: Option<String>,
    pub last_checked_at: Option<String>,
    pub resolved_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create a new known issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKnownIssueRequest {
    pub title: String,
    pub description: String,
    pub category: IssueCategory,
    pub scope_type: ScopeType,
    pub scope_value: Option<String>,
    pub scope_tags: Option<Vec<String>>,
    pub detection_method: DetectionMethod,
    pub detection_config: Option<serde_json::Value>,
    pub pattern_template_id: Option<String>,
    pub reproduction_context: Option<String>,
    pub trigger_conditions: Option<Vec<String>>,
    pub severity: IssueSeverity,
    pub provenance: Option<IssueProvenance>,
    pub source_finding_ids: Option<Vec<String>>,
    pub source_task_run_id: Option<String>,
    pub verification_hint: Option<String>,
    pub verification_step_template: Option<serde_json::Value>,
}

/// Request to update an existing known issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateKnownIssueRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<IssueCategory>,
    pub scope_type: Option<ScopeType>,
    pub scope_value: Option<String>,
    pub scope_tags: Option<Vec<String>>,
    pub detection_method: Option<DetectionMethod>,
    pub detection_config: Option<serde_json::Value>,
    pub pattern_template_id: Option<String>,
    pub reproduction_context: Option<String>,
    pub trigger_conditions: Option<Vec<String>>,
    pub severity: Option<IssueSeverity>,
    pub status: Option<IssueStatus>,
    pub confidence: Option<f64>,
    pub verification_hint: Option<String>,
    pub verification_step_template: Option<serde_json::Value>,
}

/// Query parameters for listing known issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListKnownIssuesQuery {
    pub scope_type: Option<String>,
    pub scope_value: Option<String>,
    pub category: Option<String>,
    pub severity: Option<String>,
    pub status: Option<String>,
    /// Convenience: equivalent to scope_type=spec + scope_value=<id>
    pub spec_id: Option<String>,
}

/// An issue pattern template for reusable detection strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuePatternTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub detection_type: String,
    pub step_template: Option<serde_json::Value>,
    pub ai_prompt_template: Option<String>,
    pub parameters: Vec<TemplateParameter>,
    pub built_in: bool,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create a new pattern template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePatternTemplateRequest {
    pub name: String,
    pub description: String,
    pub category: String,
    pub detection_type: String,
    pub ai_prompt_template: Option<String>,
    pub parameters: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    pub default: Option<serde_json::Value>,
}
