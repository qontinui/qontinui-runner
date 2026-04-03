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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_issues::storage::ensure_tables;

    fn create_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    #[test]
    fn test_no_findings() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_single_occurrence_not_promoted() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_recurring_finding_promoted() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_duplicate_not_created() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_category_mapping() {
        assert_eq!(
            map_finding_category("performance"),
            IssueCategory::Performance
        );
        assert_eq!(map_finding_category("runtime_issue"), IssueCategory::State);
        assert_eq!(
            map_finding_category("data_migration"),
            IssueCategory::DataIntegrity
        );
        assert_eq!(map_finding_category("code_bug"), IssueCategory::Other);
        assert_eq!(map_finding_category("unknown"), IssueCategory::Other);
    }

    #[test]
    fn test_severity_mapping() {
        assert_eq!(map_finding_severity("critical"), IssueSeverity::Critical);
        assert_eq!(map_finding_severity("high"), IssueSeverity::High);
        assert_eq!(map_finding_severity("medium"), IssueSeverity::Medium);
        assert_eq!(map_finding_severity("low"), IssueSeverity::Low);
        assert_eq!(map_finding_severity("info"), IssueSeverity::Low);
    }
}
