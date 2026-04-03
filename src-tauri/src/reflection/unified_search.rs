//! Unified search across all qontinui data stores.
//!
//! Combines SQL text search + vector similarity for findings, fixes,
//! knowledge, errors, rules, and components into a single ranked result list.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub snippet: String,
    pub relevance_score: f64,
    pub source_table: String,
}

/// Search across all data stores with a text query.
/// Results are ranked by text match relevance.
pub fn unified_search(query: &str, limit: usize) -> Result<Vec<UnifiedSearchResult>, String> {
    Err("SQLite removed".to_string())
}

fn search_findings(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_fixes(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_knowledge(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_errors(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_rules(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_workflows(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

fn search_ui_elements(pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    // SQLite removed - no-op
}

/// Truncate a string to a maximum character length, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let long = "a".repeat(200);
        let result = truncate_str(&long, 120);
        assert!(result.ends_with("..."));
        // 120 chars + "..."
        assert_eq!(result.len(), 123);
    }

    #[test]
    fn test_truncate_str_exact() {
        let exact = "a".repeat(120);
        assert_eq!(truncate_str(&exact, 120), exact);
    }
}
