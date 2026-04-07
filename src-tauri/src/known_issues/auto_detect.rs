//! Auto-detection of recurring findings that should become known issues.
//!
//! When the same finding (by `signature_hash`) recurs across multiple
//! distinct task runs, this module automatically promotes it to a
//! persistent `KnownIssue`.

use tracing::{debug, info, warn};

use crate::known_issues::storage::insert_known_issue;
use crate::known_issues::types::{
    CreateKnownIssueRequest, DetectionMethod, IssueCategory, IssueProvenance, IssueSeverity,
    ScopeType,
};

/// Check findings from the current task run and promote any recurring ones
/// (appearing in 2+ distinct task runs) to known issues.
///
/// Returns the IDs of any newly created known issues.
pub fn check_and_promote_recurring_findings(task_run_id: &str) -> Result<Vec<String>, String> {
    Err("SQLite removed".to_string())
}

/// Map a finding category string to an IssueCategory.
fn map_finding_category(category: &str) -> IssueCategory {
    match category {
        "performance" => IssueCategory::Performance,
        "runtime_issue" => IssueCategory::State,
        "security" => IssueCategory::Other,
        "code_bug" => IssueCategory::Other,
        "config_issue" => IssueCategory::Other,
        "test_issue" => IssueCategory::Other,
        "todo" => IssueCategory::Other,
        "enhancement" => IssueCategory::Other,
        "documentation" => IssueCategory::Other,
        "already_fixed" => IssueCategory::Other,
        "expected_behavior" => IssueCategory::Other,
        "warning" => IssueCategory::Other,
        "data_migration" => IssueCategory::DataIntegrity,
        _ => IssueCategory::Other,
    }
}

/// Map a finding severity string to an IssueSeverity.
fn map_finding_severity(severity: &str) -> IssueSeverity {
    match severity {
        "critical" => IssueSeverity::Critical,
        "high" => IssueSeverity::High,
        "medium" => IssueSeverity::Medium,
        "low" => IssueSeverity::Low,
        "info" => IssueSeverity::Low,
        _ => IssueSeverity::Medium,
    }
}

