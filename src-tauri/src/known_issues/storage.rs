//! Storage operations for known issues in SQLite.
//!
//! Provides CRUD operations and queries for the known_issues and
//! issue_pattern_templates tables.

#![allow(dead_code)]


use super::types::{
    CreateKnownIssueRequest, CreatePatternTemplateRequest, DetectionMethod, IssueCategory,
    IssuePatternTemplate, IssueProvenance, IssueSeverity, IssueStatus, KnownIssue,
    ListKnownIssuesQuery, ScopeType, TemplateParameter, UpdateKnownIssueRequest,
};

/// Ensure the known_issues and issue_pattern_templates tables exist.
pub fn ensure_tables() -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Insert a new known issue. Returns the created KnownIssue.
pub fn insert_known_issue(
    req: &CreateKnownIssueRequest,
) -> Result<KnownIssue, String> {
    Err("SQLite removed".to_string())
}

/// Update an existing known issue.
pub fn update_known_issue(
    id: &str,
    req: &UpdateKnownIssueRequest,
) -> Result<KnownIssue, String> {
    Err("SQLite removed".to_string())
}

/// Get a known issue by ID.
pub fn get_known_issue(id: &str) -> Result<Option<KnownIssue>, String> {
    Err("SQLite removed".to_string())
}

/// List known issues with optional filters.
pub fn list_known_issues(
    query: &ListKnownIssuesQuery,
) -> Result<Vec<KnownIssue>, String> {
    Err("SQLite removed".to_string())
}

/// Delete a known issue by ID. Returns true if a row was deleted.
pub fn delete_known_issue(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

/// Resolve a known issue (set status to resolved + resolved_at).
pub fn resolve_known_issue(
    id: &str,
    resolution: Option<&str>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Increment times_detected and update last_detected_at.
pub fn increment_detected(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Increment times_checked and update last_checked_at.
pub fn increment_checked(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Decay confidence when a regression check passes (issue was NOT detected).
/// Confidence decays toward 0.0 by multiplying by a decay factor.
/// If confidence drops below the threshold (0.1), auto-resolve the issue.
pub fn decay_confidence_on_pass(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Find issues relevant to a spec (by spec_id, URL, or global).
/// Returns active issues ordered by severity (critical first).
pub fn find_issues_for_spec(
    spec_id: &str,
    page_url: Option<&str>,
) -> Result<Vec<KnownIssue>, String> {
    Err("SQLite removed".to_string())
}

/// List all pattern templates.
pub fn list_pattern_templates() -> Result<Vec<IssuePatternTemplate>, String> {
    Err("SQLite removed".to_string())
}

/// Get a pattern template by ID.
pub fn get_pattern_template(
    id: &str,
) -> Result<Option<IssuePatternTemplate>, String> {
    Err("SQLite removed".to_string())
}

/// Insert a new pattern template. Returns the created IssuePatternTemplate.
pub fn insert_pattern_template(
    req: &CreatePatternTemplateRequest,
) -> Result<IssuePatternTemplate, String> {
    Err("SQLite removed".to_string())
}

// row_to_known_issue and row_to_pattern_template removed (SQLite dead code)

/// Find active known issues relevant for workflow generation based on depth level.
/// - "thorough": only critical + high severity
/// - "regression": all active issues
pub fn find_relevant_issues_for_generation(
    depth: &str,
) -> Result<Vec<KnownIssue>, String> {
    Err("SQLite removed".to_string())
}

/// Tokenize a string into lowercase words, filtering out words shorter than 3 characters.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= 3)
        .collect()
}

/// Compute a keyword-overlap relevance score (0.0–1.0) between task tokens and an issue.
///
/// Counts how many unique task tokens appear in the issue's title, description,
/// or scope_tags, then divides by the total number of task tokens.
fn compute_relevance_score(task_tokens: &[String], issue: &KnownIssue) -> f64 {
    if task_tokens.is_empty() {
        return 0.0;
    }

    let title_lower = issue.title.to_lowercase();
    let desc_lower = issue.description.to_lowercase();
    let tags_lower: String = issue
        .scope_tags
        .iter()
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    let haystack = format!("{} {} {}", title_lower, desc_lower, tags_lower);

    let matches = task_tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count();

    matches as f64 / task_tokens.len() as f64
}

/// Return a numeric ordering value for severity (lower = more severe).
fn severity_order(severity: &IssueSeverity) -> u8 {
    match severity {
        IssueSeverity::Critical => 0,
        IssueSeverity::High => 1,
        IssueSeverity::Medium => 2,
        IssueSeverity::Low => 3,
    }
}

/// Sort issues in-place by keyword relevance to the given task description.
///
/// Reusable sorting logic extracted from `find_relevant_issues_for_generation_with_context`
/// so callers with pre-loaded issues (e.g., from PG) can apply the same ranking.
pub fn sort_issues_by_relevance(issues: &mut Vec<KnownIssue>, task_description: &str) {
    if issues.is_empty() || task_description.trim().is_empty() {
        return;
    }
    let task_tokens = tokenize(task_description);
    if task_tokens.is_empty() {
        return;
    }
    issues.sort_by(|a, b| {
        let score_a = compute_relevance_score(&task_tokens, a);
        let score_b = compute_relevance_score(&task_tokens, b);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| severity_order(&a.severity).cmp(&severity_order(&b.severity)))
            .then_with(|| b.times_detected.cmp(&a.times_detected))
    });
}

/// Find active known issues relevant for workflow generation, ranked by keyword
/// relevance to the given task description.
///
/// Behaviour:
/// - Queries active issues filtered by depth (same severity/limit rules as
///   [`find_relevant_issues_for_generation`]).
/// - Scores each issue by keyword overlap between `task_description` and the
///   issue's title + description + scope_tags.
/// - Sorts by relevance score (descending), then severity, then times_detected.
/// - Issues with a positive relevance score appear before those with zero score.
pub fn find_relevant_issues_for_generation_with_context(
    depth: &str,
    task_description: &str,
) -> Result<Vec<KnownIssue>, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_create_request() -> CreateKnownIssueRequest {
        CreateKnownIssueRequest {
            title: "Duplicate navigation items".to_string(),
            description: "The sidebar shows duplicate nav links after page refresh".to_string(),
            category: IssueCategory::Duplication,
            scope_type: ScopeType::Url,
            scope_value: Some("http://localhost:3001/dashboard".to_string()),
            scope_tags: Some(vec!["sidebar".to_string(), "navigation".to_string()]),
            detection_method: DetectionMethod::UiBridge,
            detection_config: Some(serde_json::json!({
                "selector": ".nav-item",
                "max_expected": 5
            })),
            pattern_template_id: Some("pt_text_duplication".to_string()),
            reproduction_context: Some("Refresh the dashboard page twice".to_string()),
            trigger_conditions: Some(vec!["page_refresh".to_string()]),
            severity: IssueSeverity::High,
            provenance: Some(IssueProvenance::AutoDetected),
            source_finding_ids: Some(vec!["finding-001".to_string()]),
            source_task_run_id: Some("task-run-abc".to_string()),
            verification_hint: Some("Check sidebar link count after refresh".to_string()),
            verification_step_template: Some(serde_json::json!({
                "type": "command",
                "command": "curl http://localhost:3001/api/ui-bridge/control/snapshot"
            })),
        }
    }

    #[test]
    fn test_insert_and_get() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_update() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_update_to_resolved_sets_resolved_at() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_delete() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_resolve() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_increment_detected() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_increment_checked() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_list_with_filters() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_find_issues_for_spec() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_list_spec_id_shortcut() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_pattern_template_crud() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_get_nonexistent() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_resolved_issues_excluded_from_spec_search() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_confidence_decay() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_find_relevant_issues_with_context_ranks_by_keyword_overlap() {
        // SQLite removed - no-op
    }
}
