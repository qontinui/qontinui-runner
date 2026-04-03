//! Storage operations for findings in SQLite.
//!
//! Provides CRUD operations and queries for the task_run_findings table.

#![allow(dead_code)]

use super::types::{
    Finding, FindingActionType, FindingCategory, FindingCodeContext, FindingSeverity,
    FindingStatus, FindingSummary, FindingUserInput, ParsedFinding,
};

/// Insert a new finding into the database
pub fn insert_finding(
    task_run_id: &str,
    session_num: u32,
    parsed: &ParsedFinding,
) -> Result<Finding, String> {
    Err("SQLite removed".to_string())
}

/// Find a finding by signature hash (for deduplication)
pub fn find_by_signature(
    task_run_id: &str,
    signature_hash: &str,
) -> Result<Option<Finding>, String> {
    Err("SQLite removed".to_string())
}

/// Get a finding by ID
pub fn get_finding(id: &str) -> Result<Option<Finding>, String> {
    Err("SQLite removed".to_string())
}

/// Get all findings for a task run
pub fn get_findings_for_task(task_run_id: &str) -> Result<Vec<Finding>, String> {
    Err("SQLite removed".to_string())
}

/// Get findings by status for a task run
pub fn get_findings_by_status(
    task_run_id: &str,
    status: &FindingStatus,
) -> Result<Vec<Finding>, String> {
    Err("SQLite removed".to_string())
}

/// Update finding status
pub fn update_finding_status(
    id: &str,
    status: &FindingStatus,
    resolution: Option<&str>,
    session_num: Option<u32>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Set user response for a finding
pub fn set_user_response(id: &str, response: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Format findings for inclusion in a continuation prompt.
///
/// This creates a structured section showing:
/// - Resolved findings (so AI doesn't re-report them)
/// - Outstanding findings (still need to be addressed)
/// - Findings needing user input (with any responses)
///
/// This prevents the AI from re-detecting already-resolved issues and
/// provides context about what work remains.
pub fn format_findings_for_continuation_prompt(task_run_id: &str) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

/// Format code location for display
fn format_location(code_context: &Option<FindingCodeContext>) -> String {
    match code_context {
        Some(ctx) => {
            let file = ctx.file.as_deref().unwrap_or("");
            let line = ctx.line.map(|l| format!(":{}", l)).unwrap_or_default();
            if file.is_empty() {
                String::new()
            } else {
                format!(" @ `{}{}`", file, line)
            }
        }
        None => String::new(),
    }
}

/// Truncate description to max length, adding ellipsis if needed
fn truncate_description(desc: &str, max_len: usize) -> String {
    if desc.len() <= max_len {
        desc.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !desc.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &desc[..end])
    }
}

/// Get summary statistics for a task run
pub fn get_finding_summary(task_run_id: &str) -> Result<FindingSummary, String> {
    Err("SQLite removed".to_string())
}

/// Normalize a finding title for deduplication.
/// AI-generated titles often vary in case, whitespace, and minor wording.
fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compute a signature hash for deduplication.
/// Titles are normalized (lowercased, whitespace-collapsed) to catch near-duplicates
/// where the AI rephrases the same issue with minor variations.
fn compute_signature_hash(
    category: &FindingCategory,
    title: &str,
    file: Option<&str>,
    line: Option<u32>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let normalized_title = normalize_title(title);

    let mut hasher = DefaultHasher::new();
    category.as_str().hash(&mut hasher);
    normalized_title.hash(&mut hasher);
    file.hash(&mut hasher);
    line.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

// row_to_finding removed (SQLite dead code)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get_finding() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_deduplication() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_update_status() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_finding_summary() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_user_response() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_insert_resolved_finding() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_continuation_prompt_formatting() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_continuation_prompt_empty() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_continuation_prompt_with_user_response() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_normalize_title() {
        assert_eq!(normalize_title("Hello World"), "hello world");
        assert_eq!(normalize_title("  Extra   Spaces  "), "extra spaces");
        assert_eq!(
            normalize_title("ESLint verification step"),
            "eslint verification step"
        );
        assert_eq!(
            normalize_title("ESLint Verification Step"),
            normalize_title("eslint verification step")
        );
    }

    #[test]
    fn test_dedup_catches_case_variations() {
        // SQLite removed - no-op
    }
}
