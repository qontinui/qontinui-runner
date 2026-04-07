//! Step Type Knowledge — Per-step-type best practices and pitfalls.
//!
//! Two-layer knowledge system:
//! - **Universal** — best practices and pitfalls per step type (ships as seed data)
//! - **System-specific** — environment-specific patterns learned over time
//!
//! Knowledge is stored in PostgreSQL (see `database/pg/step_type_knowledge.rs`),
//! injected into the Builder Agent's prompt filtered to only the relevant
//! step types. This module provides the shared types and prompt-formatting helpers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
// Prompt formatting
// ============================================================================

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

