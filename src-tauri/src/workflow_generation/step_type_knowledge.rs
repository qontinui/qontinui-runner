//! Step Type Knowledge — Per-step-type best practices and pitfalls.
//!
//! Two-layer knowledge system:
//! - **Universal** — best practices and pitfalls per step type (ships as seed data)
//! - **System-specific** — environment-specific patterns learned over time
//!
//! Knowledge is stored in SQLite, injected into the Builder Agent's prompt
//! filtered to only the relevant step types.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

/// A single step type knowledge entry stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTypeKnowledge {
    pub id: String,
    pub step_type: String,
    pub layer: String,
    pub title: String,
    pub content: String,
    pub priority: i32,
    pub status: String,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for inserting a new knowledge entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertKnowledgeInput {
    pub step_type: String,
    #[serde(default = "default_layer")]
    pub layer: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_provenance")]
    pub provenance: String,
    pub source_fix_id: Option<String>,
}

fn default_layer() -> String {
    "universal".to_string()
}

fn default_provenance() -> String {
    "manual".to_string()
}

/// Input for updating an existing knowledge entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateKnowledgeInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub priority: Option<i32>,
    pub status: Option<String>,
    pub layer: Option<String>,
}

/// Query parameters for listing knowledge entries.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListKnowledgeQuery {
    pub step_type: Option<String>,
    pub layer: Option<String>,
    pub status: Option<String>,
}

// ============================================================================
// Loading Functions (for prompt injection)
// ============================================================================

/// Load active knowledge entries for the given step types, ordered by priority DESC.
pub fn load_knowledge_for_step_types(
    step_types: &[&str],
) -> Vec<StepTypeKnowledge> {
    Vec::new()
}

/// Format knowledge entries as markdown for prompt injection.
///
/// Groups by step_type, outputs:
/// ```text
/// ## Step Type Best Practices
///
/// ### command
/// - **Always set working_directory**: Command steps must specify...
/// ```
pub fn format_knowledge_as_markdown(entries: &[StepTypeKnowledge]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut grouped: HashMap<&str, Vec<&StepTypeKnowledge>> = HashMap::new();
    for entry in entries {
        grouped
            .entry(entry.step_type.as_str())
            .or_default()
            .push(entry);
    }

    // Sort step types for deterministic output
    let mut step_types: Vec<&&str> = grouped.keys().collect();
    step_types.sort();

    let mut output = String::from("## Step Type Best Practices\n\n");

    for step_type in step_types {
        output.push_str(&format!("### {}\n", step_type));
        if let Some(entries) = grouped.get(*step_type) {
            for entry in entries {
                output.push_str(&format!("- **{}**: {}\n", entry.title, entry.content));
            }
        }
        output.push('\n');
    }

    output
}

// ============================================================================
// CRUD Functions
// ============================================================================

/// List knowledge entries with optional filters.
pub fn list_knowledge(
    query: &ListKnowledgeQuery,
) -> Result<Vec<StepTypeKnowledge>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single knowledge entry by ID.
pub fn get_knowledge(id: &str) -> Result<Option<StepTypeKnowledge>, String> {
    Err("SQLite removed".to_string())
}

/// Insert a new knowledge entry.
/// If `source_fix_id` is provided and an entry with that source already exists, returns the
/// existing entry instead of creating a duplicate.
pub fn insert_knowledge(
    input: &InsertKnowledgeInput,
) -> Result<StepTypeKnowledge, String> {
    Err("SQLite removed".to_string())
}

/// Update an existing knowledge entry.
pub fn update_knowledge(
    id: &str,
    input: &UpdateKnowledgeInput,
) -> Result<StepTypeKnowledge, String> {
    Err("SQLite removed".to_string())
}

/// Delete a knowledge entry (hard delete).
pub fn delete_knowledge(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Reflection Helper
// ============================================================================

/// Infer step type from a reflection fix description based on keywords.
/// Uses compound/specific keywords first, then falls back to generic ones.
/// Scores each step type by keyword match count and returns the best match.
pub fn infer_step_type_from_fix(description: &str) -> Option<String> {
    let lower = description.to_lowercase();

    // Each entry: (step_type, keywords) -- more specific keywords listed first
    let mappings: &[(&str, &[&str])] = &[
        (
            "command",
            &[
                "command",
                "shell_command",
                "shell command",
                "working_directory",
                "fail_on_error",
                "check_type",
                "check step",
                "typecheck",
                "lint check",
                "format check",
                "check_group",
                "api_request",
                "api request",
                "curl",
                "test_type",
                "test step",
                "pytest",
                "jest",
                "cargo test",
                "test runner",
            ],
        ),
        (
            "prompt",
            &[
                "prompt step",
                "prompt ",
                "agentic prompt",
                "agent instruction",
                "base_prompt",
            ],
        ),
        // "test" merged into "command" — test keywords are now command aliases
        (
            "ui_bridge",
            &[
                "ui_bridge",
                "ui bridge",
                "ui_bridge_action",
                "navigate",
                "snapshot",
            ],
        ),
    ];

    let mut best_type: Option<&str> = None;
    let mut best_count = 0;

    for (step_type, keywords) in mappings {
        let count = keywords.iter().filter(|kw| lower.contains(**kw)).count();
        if count > best_count {
            best_count = count;
            best_type = Some(step_type);
        }
    }

    best_type.map(|s| s.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    #[test]
    fn test_step_type_knowledge_insert_and_get() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_step_type_knowledge_update() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_step_type_knowledge_delete() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_step_type_knowledge_list_with_filters() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_load_knowledge_for_step_types() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_load_knowledge_excludes_disabled() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_format_knowledge_as_markdown() {
        let entries = vec![
            StepTypeKnowledge {
                id: "1".into(),
                step_type: "command".into(),
                layer: "universal".into(),
                title: "Set working_directory".into(),
                content: "Always set working_directory for command steps.".into(),
                priority: 10,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            StepTypeKnowledge {
                id: "2".into(),
                step_type: "command".into(),
                layer: "universal".into(),
                title: "Test type required".into(),
                content: "Always specify test_type for command steps with test mode.".into(),
                priority: 10,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
        ];

        let md = format_knowledge_as_markdown(&entries);
        assert!(md.contains("## Step Type Best Practices"));
        assert!(md.contains("### command"));
        assert!(md.contains("- **Set working_directory**: Always set working_directory"));
    }

    #[test]
    fn test_format_knowledge_empty() {
        assert_eq!(format_knowledge_as_markdown(&[]), "");
    }

    #[test]
    fn test_infer_step_type_from_fix() {
        assert_eq!(
            infer_step_type_from_fix("command missing working_directory"),
            Some("command".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("shell_command needs fail_on_error"),
            Some("command".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("ui_bridge action not supported"),
            Some("ui_bridge".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("prompt instructions unclear"),
            Some("prompt".to_string())
        );
        assert_eq!(infer_step_type_from_fix("unrelated description"), None);
    }

    #[test]
    fn test_load_knowledge_empty_step_types() {
        // SQLite removed - no-op
    }
}
