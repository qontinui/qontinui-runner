//! Finding types for AI-detected issues.
//!
//! The runner's finding enums are re-exported from the canonical
//! `qontinui_types::task_run::TaskRunFinding*` enums in the schemas crate.
//! Keeping a single source of truth guarantees that variants the runner
//! emits always exist in the generated types the backend consumes (and in
//! the corresponding PG ENUMs), so drift is impossible at compile time.
//!
//! The Tauri event payload structs (`Finding`, `FindingCodeContext`,
//! `FindingUserInput`) are re-exported from `qontinui_runner_lib::tauri_event_payloads`
//! so the JSON-Schema → TypeScript pipeline (`schema_export::export_all_schemas`)
//! sees the same Rust types the binary emits over the `finding_detected` /
//! `finding_resolved` channels.
//!
//! The `as_str` / `from_str` / `is_terminal` helpers that runner code
//! depends on are provided as an extension trait below, so callers keep
//! calling `finding.category.as_str()` etc. unchanged. Free-function
//! wrappers are also exposed for explicit call sites.

use serde::{Deserialize, Serialize};

pub use qontinui_runner_lib::tauri_event_payloads::{
    Finding, FindingCodeContext, FindingUserInput,
};

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

// `Finding`, `FindingCodeContext`, `FindingUserInput` are re-exported above
// from `qontinui_runner_lib::tauri_event_payloads`. That module is the wire-
// format source of truth and is what the schema export pipeline binds.

/// Inherent helpers for the [`Finding`] payload type.
///
/// Provided as an extension trait because the `Finding` struct lives in the
/// library crate (`qontinui_runner_lib::tauri_event_payloads`) and these
/// helpers depend on the runner-local `FindingCategoryExt` trait below.
/// Call sites are unaffected: `finding.compute_signature_hash()` works as
/// long as `FindingExt` is in scope.
pub trait FindingExt {
    fn compute_signature_hash(&self) -> String;
}

impl FindingExt for Finding {
    fn compute_signature_hash(&self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that hand-written `as_str()` values match serde's
    /// `rename_all = "snake_case"` output, and that `from_str` round-trips
    /// correctly.  If a variant is added to the schemas crate but the ext
    /// trait returns a different string, this test catches it.
    macro_rules! assert_serde_parity {
        ($enum_ty:ty, $ext_trait:ident, [$($variant:expr),+ $(,)?]) => {
            $(
                {
                    let v: $enum_ty = $variant;
                    let serde_str = serde_json::to_value(&v)
                        .expect("serde_json::to_value failed")
                        .as_str()
                        .expect("serde value was not a string")
                        .to_owned();
                    assert_eq!(
                        <$enum_ty as $ext_trait>::as_str(&v),
                        serde_str.as_str(),
                        "as_str mismatch for {:?}",
                        v,
                    );
                    let round_trip: $enum_ty = serde_json::from_value(
                        serde_json::Value::String(
                            <$enum_ty as $ext_trait>::as_str(&v).to_owned(),
                        ),
                    )
                    .unwrap_or_else(|e| panic!("from_str round-trip failed for {:?}: {}", v, e));
                    assert_eq!(v, round_trip, "round-trip mismatch for {:?}", v);
                }
            )+
        };
    }

    #[test]
    fn finding_category_serde_parity() {
        use FindingCategory::*;
        assert_serde_parity!(
            FindingCategory,
            FindingCategoryExt,
            [
                CodeBug,
                Security,
                Performance,
                Todo,
                Enhancement,
                ConfigIssue,
                TestIssue,
                Documentation,
                RuntimeIssue,
                AlreadyFixed,
                ExpectedBehavior,
                Warning,
                DataMigration,
            ]
        );
    }

    #[test]
    fn finding_severity_serde_parity() {
        use FindingSeverity::*;
        assert_serde_parity!(
            FindingSeverity,
            FindingSeverityExt,
            [Critical, High, Medium, Low, Info,]
        );
    }

    #[test]
    fn finding_status_serde_parity() {
        use FindingStatus::*;
        assert_serde_parity!(
            FindingStatus,
            FindingStatusExt,
            [Detected, InProgress, NeedsInput, Resolved, WontFix, Deferred,]
        );
    }

    #[test]
    fn finding_action_type_serde_parity() {
        use FindingActionType::*;
        assert_serde_parity!(
            FindingActionType,
            FindingActionTypeExt,
            [AutoFix, NeedsUserInput, Manual, Informational,]
        );
    }
}
