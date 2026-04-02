//! Similar workflow retrieval for workflow generation context.
//!
//! Finds semantically similar existing workflows to use as reference examples
//! when generating new workflows. Includes special handling for ground truth
//! workflows that provide full JSON examples.

use serde::{Deserialize, Serialize};

use crate::database::embeddings::{blob_to_vector, cosine_similarity};
use crate::database::Connection;

/// A similar workflow found by vector search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarWorkflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub setup_step_count: usize,
    pub verification_step_count: usize,
    pub agentic_step_count: usize,
    pub completion_step_count: usize,
    pub similarity: f32,
    /// Full workflow JSON — only populated for ground_truth category workflows.
    pub full_json: Option<String>,
}

/// Find workflows similar to the given description.
///
/// Uses the description embedding column for cosine similarity.
/// Filters by category if provided and excludes meta-workflows.
pub fn find_similar_workflows(
    conn: &Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<SimilarWorkflow>, String> {
    Err("SQLite removed".to_string())
}

/// Find ground truth reference workflows matching the given description.
///
/// Uses keyword overlap scoring to find relevant GT workflows. This works
/// without the embedding service. If embeddings are available, they boost
/// the score. GT workflows have category = 'ground_truth'.
pub fn find_gt_reference_workflows(
    conn: &Connection,
    description: &str,
    query_embedding: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<SimilarWorkflow>, String> {
    Err("SQLite removed".to_string())
}

/// Format similar workflows as markdown for prompt injection.
///
/// Shows top 3 as reference examples with structure info.
pub fn format_similar_workflows(similar: &[SimilarWorkflow]) -> String {
    if similar.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Similar Workflows (for reference)\n\n");
    output.push_str(
        "These existing workflows are similar to what you're generating. \
         Use them as structural references, but do NOT copy them — generate fresh content.\n\n",
    );

    for (i, w) in similar.iter().take(3).enumerate() {
        output.push_str(&format!(
            "### Reference {}: {} (similarity: {:.0}%)\n",
            i + 1,
            w.name,
            w.similarity * 100.0,
        ));
        output.push_str(&format!("- **Category**: {}\n", w.category));
        output.push_str(&format!(
            "- **Structure**: {} setup, {} verification, {} agentic, {} completion steps\n",
            w.setup_step_count,
            w.verification_step_count,
            w.agentic_step_count,
            w.completion_step_count,
        ));
        if !w.description.is_empty() {
            let desc_preview = if w.description.len() > 200 {
                format!("{}...", &w.description[..200])
            } else {
                w.description.clone()
            };
            output.push_str(&format!("- **Description**: {}\n", desc_preview));
        }
        output.push('\n');
    }

    output
}

/// Format ground truth reference workflows with full JSON examples.
///
/// Includes the complete workflow JSON so the builder can closely match
/// the expected structure and tool choices.
pub fn format_gt_references(gt_workflows: &[SimilarWorkflow]) -> String {
    if gt_workflows.is_empty() {
        return String::new();
    }

    let mut output = String::from(
        "## Ground Truth Reference Workflows\n\n\
         The following are **verified correct** workflows for similar tasks. \
         Your output MUST closely match these examples in:\n\
         - **Tool choices** (use the same tools: ruff, eslint, tsc, prettier, cargo, mypy, black, clippy)\n\
         - **Step structure** (setup_steps=[], completion_steps=[], only check+gate in verification)\n\
         - **Check types** (lint, format, typecheck — match the tool to the correct check_type)\n\n\
         Adapt paths and names for the target repository, but use the SAME tools and structure.\n\n",
    );

    for (i, w) in gt_workflows.iter().enumerate() {
        output.push_str(&format!(
            "### Ground Truth Example {} — {} (similarity: {:.0}%)\n\n",
            i + 1,
            w.name,
            w.similarity * 100.0,
        ));
        output.push_str(&format!("**Description**: {}\n\n", w.description));

        if let Some(ref json) = w.full_json {
            output.push_str("**Workflow JSON** (verified correct):\n```json\n");
            output.push_str(json);
            output.push_str("\n```\n\n");
        }
    }

    output
}
