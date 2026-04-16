//! Finding types for AI-detected issues.
//!
//! The runner's finding enums are re-exported from the canonical
//! `qontinui_types::task_run::TaskRunFinding*` enums in the schemas crate.
//! Keeping a single source of truth guarantees that variants the runner
//! emits always exist in the generated types the backend consumes (and in
//! the corresponding PG ENUMs), so drift is impossible at compile time.
//!
//! The `as_str` / `from_str` / `is_terminal` helpers that runner code
//! depends on are provided as an extension trait below, so callers keep
//! calling `finding.category.as_str()` etc. unchanged. Free-function
//! wrappers are also exposed for explicit call sites.

use serde::{Deserialize, Serialize};

pub use qontinui_types::task_run::{
    TaskRunFindingActionType as FindingActionType, TaskRunFindingCategory as FindingCategory,
    TaskRunFindingSeverity as FindingSeverity, TaskRunFindingStatus as FindingStatus,
};

// ============================================================================
// Extension traits — static exhaustiveness for snake_case mapping helpers.
// ============================================================================

/// `as_str` / `from_str` helpers on [`FindingCategory`].
pub trait FindingCategoryExt: Sized {
    fn as_str(&self) -> &'static str;
    fn from_str(s: &str) -> Option<Self>;
}

impl FindingCategoryExt for FindingCategory {
    fn as_str(&self) -> &'static str {
        match self {
            Self::CodeBug => "code_bug",
            Self::Security => "security",
            Self::Performance => "performance",
            Self::Todo => "todo",
            Self::Enhancement => "enhancement",
            Self::ConfigIssue => "config_issue",
            Self::TestIssue => "test_issue",
            Self::Documentation => "documentation",
            Self::RuntimeIssue => "runtime_issue",
            Self::AlreadyFixed => "already_fixed",
            Self::ExpectedBehavior => "expected_behavior",
            Self::Warning => "warning",
            Self::DataMigration => "data_migration",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "code_bug" => Some(Self::CodeBug),
            "security" => Some(Self::Security),
            "performance" => Some(Self::Performance),
            "todo" => Some(Self::Todo),
            "enhancement" => Some(Self::Enhancement),
            "config_issue" => Some(Self::ConfigIssue),
            "test_issue" => Some(Self::TestIssue),
            "documentation" => Some(Self::Documentation),
            "runtime_issue" => Some(Self::RuntimeIssue),
            "already_fixed" => Some(Self::AlreadyFixed),
            "expected_behavior" => Some(Self::ExpectedBehavior),
            "warning" => Some(Self::Warning),
            "data_migration" => Some(Self::DataMigration),
            _ => None,
        }
    }
}

/// `as_str` / `from_str` helpers on [`FindingSeverity`].
pub trait FindingSeverityExt: Sized {
    fn as_str(&self) -> &'static str;
    fn from_str(s: &str) -> Option<Self>;
}

impl FindingSeverityExt for FindingSeverity {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}

/// `as_str` / `from_str` / `is_terminal` helpers on [`FindingStatus`].
pub trait FindingStatusExt: Sized {
    fn as_str(&self) -> &'static str;
    fn from_str(s: &str) -> Option<Self>;
    fn is_terminal(&self) -> bool;
}

impl FindingStatusExt for FindingStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::InProgress => "in_progress",
            Self::NeedsInput => "needs_input",
            Self::Resolved => "resolved",
            Self::WontFix => "wont_fix",
            Self::Deferred => "deferred",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "detected" => Some(Self::Detected),
            "in_progress" => Some(Self::InProgress),
            "needs_input" => Some(Self::NeedsInput),
            "resolved" => Some(Self::Resolved),
            "wont_fix" => Some(Self::WontFix),
            "deferred" => Some(Self::Deferred),
            _ => None,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Resolved | Self::WontFix | Self::Deferred)
    }
}

/// `as_str` / `from_str` helpers on [`FindingActionType`].
pub trait FindingActionTypeExt: Sized {
    fn as_str(&self) -> &'static str;
    fn from_str(s: &str) -> Option<Self>;
}

impl FindingActionTypeExt for FindingActionType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::AutoFix => "auto_fix",
            Self::NeedsUserInput => "needs_user_input",
            Self::Manual => "manual",
            Self::Informational => "informational",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto_fix" => Some(Self::AutoFix),
            "needs_user_input" => Some(Self::NeedsUserInput),
            "manual" => Some(Self::Manual),
            "informational" => Some(Self::Informational),
            _ => None,
        }
    }
}

/// Code context for a finding
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FindingCodeContext {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub snippet: Option<String>,
}

/// User input request for a finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingUserInput {
    pub question: String,
    #[serde(default = "default_input_type")]
    pub input_type: String,
    pub options: Option<Vec<String>>,
}

fn default_input_type() -> String {
    "text".to_string()
}

/// A finding detected by AI analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub task_run_id: String,
    pub session_num: u32,

    #[serde(rename = "categoryId")]
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub action_type: FindingActionType,

    pub title: String,
    pub description: String,
    pub resolution: Option<String>,

    pub code_context: Option<FindingCodeContext>,
    pub signature_hash: String,

    pub user_input: Option<FindingUserInput>,
    pub user_response: Option<String>,

    pub detected_at: String,
    pub resolved_at: Option<String>,
    pub resolved_in_session: Option<u32>,
    pub updated_at: String,
}

impl Finding {
    /// Generate a signature hash for deduplication
    pub fn compute_signature_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.category.as_str().hash(&mut hasher);
        self.title.hash(&mut hasher);
        if let Some(ref ctx) = self.code_context {
            ctx.file.hash(&mut hasher);
            ctx.line.hash(&mut hasher);
        }
        format!("{:x}", hasher.finish())
    }
}

/// Summary statistics for findings in a task run
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FindingSummary {
    pub task_run_id: String,
    pub total: u32,
    pub by_category: std::collections::HashMap<String, u32>,
    pub by_severity: std::collections::HashMap<String, u32>,
    pub by_status: std::collections::HashMap<String, u32>,
    pub needs_input_count: u32,
    pub resolved_count: u32,
    pub outstanding_count: u32,
}

/// Parsed finding from AI output (before database insertion)
#[derive(Debug, Clone)]
pub struct ParsedFinding {
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub needs_input: bool,
    /// Whether this finding is marked as already resolved (via :resolved modifier)
    pub is_resolved: bool,
    pub title: String,
    pub description: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub resolution: Option<String>,
    pub question: Option<String>,
    pub options: Option<Vec<String>>,
}
