//! Types for the reflection workflow system.
//!
//! Defines the core data structures for tracking fixes applied by reflection
//! workflows and their effectiveness over time.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A fix applied by a reflection workflow after analyzing a completed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionFix {
    pub id: String,
    /// The task run that was analyzed
    pub source_task_run_id: String,
    /// The reflection workflow's own task run
    pub reflection_task_run_id: String,
    /// FK to task_run_findings (if fix addresses a specific finding)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_finding_id: Option<String>,
    /// FK to task_knowledge (if fix addresses a specific knowledge entry)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_knowledge_id: Option<String>,
    /// What kind of fix was applied
    pub fix_type: String,
    /// Human-readable description of the fix
    pub fix_description: String,
    /// File that was changed (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_changed: Option<String>,
    /// Previous value (for diffing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<String>,
    /// New value (for diffing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
    /// Confidence level of the fix
    pub confidence: String,
    /// Content hash for deduplication across reflection runs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Current status of the fix
    pub status: String,
    /// Effectiveness evaluation result (NULL until evaluated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effectiveness: Option<String>,
    /// Evidence supporting the effectiveness evaluation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effectiveness_evidence: Option<String>,
    /// When the fix was applied
    pub applied_at: String,
    /// When effectiveness was evaluated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_at: Option<String>,
    pub created_at: String,
    /// Which generation agent's output likely caused this issue
    /// (e.g. "specification", "builder", "verification", "hardener")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<String>,
}

/// Types of fixes that a reflection workflow can apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixType {
    /// Updated knowledge base content
    KnowledgeBaseUpdate,
    /// Rewrote a workflow step
    WorkflowStepRewrite,
    /// Fixed a UI selector
    SelectorFix,
    /// Updated tool configuration
    ToolConfigUpdate,
    /// Added missing context information
    ContextAddition,
    /// Clarified ambiguous instructions
    InstructionClarification,

    // --- Project-scoped fix types (project reflection) ---
    /// Environment requirements: deps, env vars, runtime versions, services
    ProjectEnvironment,
    /// Codebase structure: where tests live, framework patterns, build system
    ProjectArchitecture,
    /// Test infrastructure: runner, fixture patterns, setup/teardown
    ProjectTestPattern,
    /// Recurring friction: common failures, flaky areas, known quirks
    ProjectRecurringIssue,
}

impl FixType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::KnowledgeBaseUpdate => "knowledge_base_update",
            Self::WorkflowStepRewrite => "workflow_step_rewrite",
            Self::SelectorFix => "selector_fix",
            Self::ToolConfigUpdate => "tool_config_update",
            Self::ContextAddition => "context_addition",
            Self::InstructionClarification => "instruction_clarification",
            Self::ProjectEnvironment => "project_environment",
            Self::ProjectArchitecture => "project_architecture",
            Self::ProjectTestPattern => "project_test_pattern",
            Self::ProjectRecurringIssue => "project_recurring_issue",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "knowledge_base_update" | "kb_update" => Some(Self::KnowledgeBaseUpdate),
            "workflow_step_rewrite" | "step_rewrite" => Some(Self::WorkflowStepRewrite),
            "selector_fix" | "selector" => Some(Self::SelectorFix),
            "tool_config_update" | "tool_config" => Some(Self::ToolConfigUpdate),
            "context_addition" | "context" => Some(Self::ContextAddition),
            "instruction_clarification" | "clarification" => Some(Self::InstructionClarification),
            "project_environment" | "proj_env" => Some(Self::ProjectEnvironment),
            "project_architecture" | "proj_arch" => Some(Self::ProjectArchitecture),
            "project_test_pattern" | "proj_test" => Some(Self::ProjectTestPattern),
            "project_recurring_issue" | "proj_issue" => Some(Self::ProjectRecurringIssue),
            _ => None,
        }
    }

    /// Returns true if this is a project-scoped fix type (from project reflection).
    pub fn is_project_scoped(&self) -> bool {
        matches!(
            self,
            Self::ProjectEnvironment
                | Self::ProjectArchitecture
                | Self::ProjectTestPattern
                | Self::ProjectRecurringIssue
        )
    }
}

impl fmt::Display for FixType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Status of a reflection fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixStatus {
    /// Fix has been applied
    Applied,
    /// Fix was reverted (caused issues)
    Reverted,
    /// Fix was replaced by a newer fix
    Superseded,
}

impl FixStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Reverted => "reverted",
            Self::Superseded => "superseded",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "applied" => Some(Self::Applied),
            "reverted" => Some(Self::Reverted),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

impl fmt::Display for FixStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Effectiveness evaluation result for a reflection fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixEffectiveness {
    /// The fix resolved the issue (did not recur)
    Effective,
    /// The fix did not resolve the issue (recurred)
    Ineffective,
    /// The fix introduced new problems
    CausedRegression,
    /// Not enough data to determine effectiveness
    Inconclusive,
}

impl FixEffectiveness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Effective => "effective",
            Self::Ineffective => "ineffective",
            Self::CausedRegression => "caused_regression",
            Self::Inconclusive => "inconclusive",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "effective" => Some(Self::Effective),
            "ineffective" => Some(Self::Ineffective),
            "caused_regression" | "regression" => Some(Self::CausedRegression),
            "inconclusive" => Some(Self::Inconclusive),
            _ => None,
        }
    }
}

impl fmt::Display for FixEffectiveness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Aggregated effectiveness report for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivenessReport {
    pub workflow_name: String,
    pub total_fixes: u32,
    pub effective_count: u32,
    pub ineffective_count: u32,
    pub regression_count: u32,
    pub inconclusive_count: u32,
    pub unevaluated_count: u32,
    pub effectiveness_rate: f64,
    pub fixes: Vec<ReflectionFix>,
}

/// Input for creating a new reflection fix.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateReflectionFixInput {
    pub source_task_run_id: String,
    pub reflection_task_run_id: String,
    #[serde(default)]
    pub source_finding_id: Option<String>,
    #[serde(default)]
    pub source_knowledge_id: Option<String>,
    pub fix_type: String,
    pub fix_description: String,
    #[serde(default)]
    pub file_changed: Option<String>,
    #[serde(default)]
    pub old_value: Option<String>,
    #[serde(default)]
    pub new_value: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: String,
    /// Which generation agent's output likely caused this issue
    #[serde(default)]
    pub source_agent: Option<String>,
}

fn default_confidence() -> String {
    "medium".to_string()
}

/// Input for updating fix effectiveness.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateEffectivenessInput {
    pub effectiveness: String,
    #[serde(default)]
    pub effectiveness_evidence: Option<String>,
}

/// Input for updating fix status.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFixStatusInput {
    pub status: String,
}
