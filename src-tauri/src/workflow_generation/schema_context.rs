//! Schema Context for Workflow Generation
//!
//! Builds the AI prompt with workflow schema documentation and examples.
//! Documentation is auto-generated from the step type metadata registry
//! rather than hardcoded static strings.

use crate::database::Connection;
use crate::database::pg::PgDb;
use std::sync::Arc;

use super::relevance_filter::filter_relevant_step_types;
use super::rules;
use super::step_type_knowledge;
use super::step_type_metadata::{get_all_step_type_metadata, StepTypeMetadata};
use crate::skills::playbook_parser;
use crate::skills::SkillRegistry;

/// Build the complete schema context prompt for AI workflow generation.
///
/// Backward-compatible: uses all step types and no RAG examples.
pub fn build_schema_context() -> String {
    let all_types = get_all_step_type_metadata();
    let type_refs: Vec<&StepTypeMetadata> = all_types.iter().collect();
    let step_types_doc = generate_step_types_documentation(&type_refs);
    let phase_table = generate_phase_constraint_table(&type_refs);
    assemble_prompt(&step_types_doc, &phase_table, "", "", "", None)
}

/// Build schema context filtered by description keywords.
///
/// Reduces token usage by only including step types relevant to the description.
/// Uses no RAG examples (no DB access).
pub fn build_schema_context_for_description(description: &str) -> String {
    let all_types = get_all_step_type_metadata();
    let filtered = filter_relevant_step_types(description, all_types);
    let step_types_doc = generate_step_types_documentation(&filtered);
    let phase_table = generate_phase_constraint_table(&filtered);
    assemble_prompt(&step_types_doc, &phase_table, "", "", "", None)
}

/// Build full schema context with filtered types + RAG examples from DB.
///
/// This is the most complete version, used when a DB connection and optionally
/// a query embedding are available.
pub fn build_schema_context_full(
    description: &str,
    pg_db: Option<&Arc<PgDb>>,
    query_embedding: Option<&[f32]>,
) -> String {
    let all_types = get_all_step_type_metadata();
    let filtered = filter_relevant_step_types(description, all_types);
    let step_types_doc = generate_step_types_documentation(&filtered);
    let phase_table = generate_phase_constraint_table(&filtered);

    // Retrieve RAG examples from PG if available
    let examples_section = if let Some(pg) = pg_db {
        let pg_clone = pg.clone();
        let desc = description.to_string();
        let examples = tokio::runtime::Handle::current().block_on(async {
            pg_clone.search_unified_workflows_for_examples(&desc, 3).await.unwrap_or_default()
        });
        if !examples.is_empty() {
            // Format PG example workflows for prompt context
            let mut s = String::new();
            for (i, wf) in examples.iter().take(3).enumerate() {
                s.push_str(&format!(
                    "### Example {} — {}\n{}\n\n",
                    i + 1,
                    wf.name,
                    wf.description
                ));
            }
            s
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Load step type knowledge filtered to relevant step types via PG
    let knowledge_section = if let Some(pg) = pg_db {
        let pg_clone = pg.clone();
        let step_type_names: Vec<String> = filtered.iter().map(|m| m.step_type.to_string()).collect();
        let entries = tokio::runtime::Handle::current().block_on(async {
            let refs: Vec<&str> = step_type_names.iter().map(|s| s.as_str()).collect();
            pg_clone.load_knowledge_for_step_types(&refs).await.unwrap_or_default()
        });
        if !entries.is_empty() {
            step_type_knowledge::format_knowledge_as_markdown(&entries)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Generation confidence: requires convergence_snapshots PG migration (skipped for now)
    let confidence_section = String::new();

    assemble_prompt(
        &step_types_doc,
        &phase_table,
        &examples_section,
        &knowledge_section,
        &confidence_section,
        pg_db,
    )
}

/// Build schema context partitioned into stable and dynamic parts.
///
/// Returns `(stable_part, dynamic_part)` for prompt caching — stable content
/// gets `cache_control` breakpoints, dynamic content doesn't.
pub fn build_schema_context_partitioned(
    description: &str,
    pg_db: Option<&Arc<PgDb>>,
) -> (String, String) {
    let all_types = get_all_step_type_metadata();
    let filtered = filter_relevant_step_types(description, all_types);
    let step_types_doc = generate_step_types_documentation(&filtered);
    let phase_table = generate_phase_constraint_table(&filtered);

    // Stable part: deterministic documentation that only changes on code updates
    let stable_part = format!(
        "You are a workflow generation assistant for Qontinui Runner.\n\n\
         ## Step Types\n\n{}\n\n## Phase Constraints\n\n{}",
        step_types_doc, phase_table
    );

    // Dynamic part: DB-sourced content that varies per description
    let mut dynamic_parts: Vec<String> = Vec::new();

    if let Some(pg) = pg_db {
        let pg_clone = pg.clone();
        let desc = description.to_string();
        let examples = tokio::runtime::Handle::current().block_on(async {
            pg_clone
                .search_unified_workflows_for_examples(&desc, 3)
                .await
                .unwrap_or_default()
        });
        if !examples.is_empty() {
            let mut s = String::from("## Examples\n\n");
            for (i, wf) in examples.iter().take(3).enumerate() {
                s.push_str(&format!("### Example {} — {}\n{}\n\n", i + 1, wf.name, wf.description));
            }
            dynamic_parts.push(s);
        }
    }

    let dynamic_part = dynamic_parts.join("\n\n");
    (stable_part, dynamic_part)
}

// ============================================================================
// Stub Functions (previously SQLite-backed, now dead)
// ============================================================================

/// Generate phase constraint table from step type metadata.
fn generate_phase_constraint_table(_types: &[&StepTypeMetadata]) -> String {
    String::new()
}

/// Assemble the full generation prompt from parts.
fn assemble_prompt(
    step_types_doc: &str,
    phase_table: &str,
    examples_section: &str,
    knowledge_section: &str,
    confidence_section: &str,
    _pg_db: Option<&Arc<PgDb>>,
) -> String {
    let mut parts = vec![
        "You are a workflow generation assistant for Qontinui Runner.\n".to_string(),
    ];
    if !step_types_doc.is_empty() {
        parts.push(format!("## Step Types\n\n{}", step_types_doc));
    }
    if !phase_table.is_empty() {
        parts.push(format!("## Phase Constraints\n\n{}", phase_table));
    }
    if !examples_section.is_empty() {
        parts.push(format!("## Examples\n\n{}", examples_section));
    }
    if !knowledge_section.is_empty() {
        parts.push(format!("## Knowledge\n\n{}", knowledge_section));
    }
    if !confidence_section.is_empty() {
        parts.push(confidence_section.to_string());
    }
    parts.join("\n\n")
}

/// Build a gotchas section from known issues (SQLite removed, returns empty).
pub fn build_gotchas_section(_pg_db: Option<&Arc<PgDb>>) -> String {
    String::new()
}

/// Build rules section for a given tier (SQLite removed, returns empty).
pub fn build_rules_section_for_tier(_pg_db: Option<&Arc<PgDb>>, _tier: rules::RuleTier) -> String {
    String::new()
}

/// Format all skills for the generator prompt.
pub fn format_skills_for_generator(_registry: &SkillRegistry) -> String {
    String::new()
}

/// Format skills for the generator prompt, filtered by tags.
pub fn format_skills_for_generator_filtered(
    _registry: &SkillRegistry,
    _domain: Option<&str>,
    _tags: &[String],
    _limit: Option<usize>,
) -> String {
    String::new()
}

// ============================================================================
// Auto-Generated Documentation
// ============================================================================

/// Generate the step types documentation section from metadata.
///
/// Produces markdown blocks like:
/// ```text
/// ### command (Setup, Verification, or Completion)
/// Execute a shell command, code quality check, or test.
/// Fields:
/// - `command`: string — Shell command to execute
/// ```
pub fn generate_step_types_documentation(types: &[&StepTypeMetadata]) -> String {
    String::new()
}