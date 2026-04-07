//! In-memory ranking helpers for known issues.
//!
//! All persistent storage for known issues now lives in PostgreSQL
//! (see `database/pg/known_issues.rs`). This module only contains
//! pure sorting/scoring logic that callers apply to issues already
//! loaded from PG.

use super::types::{IssueSeverity, KnownIssue};

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
/// Callers with pre-loaded issues (e.g., from PG) apply this ranking before
/// injecting the most relevant issues into workflow generation prompts.
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
