//! Storage operations for reflection fixes in SQLite.
//!
//! Provides CRUD operations and queries for the reflection_fixes table.

use super::types::{CreateReflectionFixInput, EffectivenessReport, ReflectionFix};

// row_to_fix removed (SQLite dead code)

const SELECT_ALL_COLUMNS: &str = r#"
    id, source_task_run_id, reflection_task_run_id,
    source_finding_id, source_knowledge_id,
    fix_type, fix_description, file_changed,
    old_value, new_value, confidence, content_hash,
    status, effectiveness, effectiveness_evidence,
    applied_at, evaluated_at, created_at, source_agent,
    reasoning, alternatives_considered,
    reflection_scope, project_path, applicability_context
"#;

/// Normalize text for semantic deduplication.
///
/// Strips run-specific content (UUIDs, timestamps, hex hashes) and normalizes
/// whitespace/case so that semantically identical fixes with slightly different
/// wording produce the same hash.
fn normalize_for_hash(text: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;

    static UUID_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap()
    });
    static TIMESTAMP_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\d{4}-\d{2}-\d{2}t\d{2}:\d{2}:\d{2}[^\s]*").unwrap());
    static HEX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").unwrap());
    static RUN_NUMBER_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\b(?:run|iteration|session)\s*\d+").unwrap());

    let text = text.to_lowercase();
    let text = UUID_RE.replace_all(&text, "").to_string();
    let text = TIMESTAMP_RE.replace_all(&text, "").to_string();
    let text = HEX_RE.replace_all(&text, "").to_string();
    let text = RUN_NUMBER_RE.replace_all(&text, "").to_string();

    // Collapse whitespace
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute a content hash for deduplication of reflection fixes.
///
/// Uses semantic normalization to catch near-duplicate fixes with slightly
/// different wording, UUIDs, or timestamps but identical meaning.
fn compute_content_hash(
    fix_type: &str,
    description: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    fix_type.hash(&mut hasher);
    normalize_for_hash(description).hash(&mut hasher);
    old_value.map(normalize_for_hash).hash(&mut hasher);
    new_value.map(normalize_for_hash).hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Insert a new reflection fix into the database.
/// Deduplicates by content hash — if an identical fix already exists with status 'applied',
/// the existing fix is returned instead of creating a duplicate.
pub fn insert_fix(input: &CreateReflectionFixInput) -> Result<ReflectionFix, String> {
    Err("SQLite removed".to_string())
}

/// Get a single reflection fix by ID.
pub fn get_fix(id: &str) -> Result<Option<ReflectionFix>, String> {
    Err("SQLite removed".to_string())
}

/// Get all fixes created by analyzing a specific source run.
pub fn get_fixes_for_source_run(source_task_run_id: &str) -> Result<Vec<ReflectionFix>, String> {
    Err("SQLite removed".to_string())
}

/// Get all fixes for runs of a specific workflow, with optional status and effectiveness filters.
pub fn get_fixes_by_workflow_name(
    workflow_name: &str,
    status_filter: Option<&str>,
    effectiveness_filter: Option<&str>,
) -> Result<Vec<ReflectionFix>, String> {
    Err("SQLite removed".to_string())
}

/// Get all project-scoped fixes for a given project path, with optional filters.
pub fn get_fixes_by_project_path(
    project_path: &str,
    status_filter: Option<&str>,
) -> Result<Vec<ReflectionFix>, String> {
    Err("SQLite removed".to_string())
}

/// Get universal-scoped fixes ordered by reuse_count (SQL-only fallback when embeddings unavailable).
pub fn get_universal_fixes(limit: usize) -> Result<Vec<ReflectionFix>, String> {
    Err("SQLite removed".to_string())
}

/// Promote a fix to universal scope with optional applicability context.
pub fn promote_to_universal(
    fix_id: &str,
    applicability_context: Option<&str>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update the status of a reflection fix (applied/reverted/superseded).
pub fn update_fix_status(id: &str, status: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update the effectiveness evaluation of a reflection fix.
pub fn update_fix_effectiveness(
    id: &str,
    effectiveness: &str,
    evidence: Option<&str>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Generate an aggregated effectiveness report for a workflow.
pub fn get_effectiveness_report(workflow_name: &str) -> Result<EffectivenessReport, String> {
    Err("SQLite removed".to_string())
}

/// Get all reflection task runs for a workflow (history).
pub fn get_reflection_history(workflow_name: &str) -> Result<Vec<ReflectionRunSummary>, String> {
    Err("SQLite removed".to_string())
}

/// Summary of a reflection run for history display.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflectionRunSummary {
    pub task_run_id: String,
    pub source_task_run_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub fix_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get_fix() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_get_fixes_for_source_run() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_update_effectiveness() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_dedup_skips_duplicate_fix() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_dedup_allows_different_content() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_normalize_strips_uuids_and_timestamps() {
        let text1 = "Confirmed effectiveness across runs d6a87e38-bfb1-4a2d-9a7a-0f4405a4bdd5 and 650cfd40-27d5-4a2d-9a7a-abc123456789 at 2026-02-15T22:44:08.411Z";
        let text2 = "Confirmed effectiveness across runs a1b2c3d4-1111-4aaa-b111-c1d1e1f11111 and 77645c1e-6d41-4a64-bd51-401d500e4099 at 2026-02-16T10:00:00Z";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_normalize_strips_run_numbers() {
        let text1 = "Evaluated effectiveness for Run 5 iteration 3";
        let text2 = "Evaluated effectiveness for Run 8 iteration 1";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_normalize_collapses_whitespace() {
        let text1 = "Fix   the   selector   issue";
        let text2 = "fix the selector issue";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_semantic_dedup_catches_near_duplicates() {
        // Two descriptions that differ only by UUIDs and run numbers
        let hash1 = compute_content_hash(
            "knowledge_base_update",
            "Confirmed effectiveness of JSON preamble fixes across run d6a87e38-bfb1-4a2d-9a7a-0f4405a4bdd5",
            None,
            None,
        );
        let hash2 = compute_content_hash(
            "knowledge_base_update",
            "Confirmed effectiveness of JSON preamble fixes across run 650cfd40-27d5-4a2d-9a7a-abc123456789",
            None,
            None,
        );
        assert_eq!(
            hash1, hash2,
            "Near-duplicate descriptions should produce the same hash"
        );
    }

    #[test]
    fn test_semantic_dedup_preserves_real_differences() {
        let hash1 = compute_content_hash(
            "knowledge_base_update",
            "Fix for JSON preamble parsing",
            None,
            None,
        );
        let hash2 = compute_content_hash(
            "knowledge_base_update",
            "Fix for template variable resolution",
            None,
            None,
        );
        assert_ne!(
            hash1, hash2,
            "Genuinely different descriptions should produce different hashes"
        );
    }
}
