//! Similar workflow retrieval for workflow generation context.
//!
//! Finds semantically similar existing workflows to use as reference examples
//! when generating new workflows. Includes special handling for ground truth
//! workflows that provide full JSON examples.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::database::embeddings::{blob_to_vector, cosine_similarity};
use crate::database::pg::PgDb;

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

// ============================================================================
// PG-backed similar workflow retrieval (text-based fallback, no pgvector)
// ============================================================================

/// Find similar workflows from PG using full-text search (no pgvector required).
///
/// Falls back to text similarity via ts_rank on the name+description GIN index.
pub async fn find_similar_workflows_pg(
    pg: &Arc<PgDb>,
    description: &str,
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<SimilarWorkflow>, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    // Extract keywords from description for full-text search
    let search_terms = extract_search_terms(description);
    if search_terms.is_empty() {
        return Ok(vec![]);
    }

    let ts_query = search_terms.join(" | ");
    let lim = limit as i64;

    let rows = match category {
        Some(cat) => {
            conn.query(
                r#"SELECT id, name, COALESCE(description, '') as description, category,
                          setup_steps, verification_steps, agentic_steps, completion_steps,
                          ts_rank(to_tsvector('english', name || ' ' || COALESCE(description, '')),
                                  to_tsquery('english', $1)) as rank
                   FROM unified_workflows
                   WHERE category = $2
                     AND category != 'meta'
                     AND to_tsvector('english', name || ' ' || COALESCE(description, ''))
                         @@ to_tsquery('english', $1)
                   ORDER BY rank DESC
                   LIMIT $3"#,
                &[&ts_query, &cat, &lim],
            )
            .await
        }
        None => {
            conn.query(
                r#"SELECT id, name, COALESCE(description, '') as description, category,
                          setup_steps, verification_steps, agentic_steps, completion_steps,
                          ts_rank(to_tsvector('english', name || ' ' || COALESCE(description, '')),
                                  to_tsquery('english', $1)) as rank
                   FROM unified_workflows
                   WHERE category != 'meta'
                     AND to_tsvector('english', name || ' ' || COALESCE(description, ''))
                         @@ to_tsquery('english', $1)
                   ORDER BY rank DESC
                   LIMIT $2"#,
                &[&ts_query, &lim],
            )
            .await
        }
    }
    .map_err(|e| format!("PG find_similar_workflows: {}", e))?;

    Ok(rows
        .iter()
        .map(|r| {
            let rank: f32 = r.get::<_, f64>(8) as f32;
            SimilarWorkflow {
                id: r.get(0),
                name: r.get(1),
                description: r.get(2),
                category: r.get(3),
                setup_step_count: count_json_array(r.get::<_, String>(4).as_str()),
                verification_step_count: count_json_array(r.get::<_, String>(5).as_str()),
                agentic_step_count: count_json_array(r.get::<_, String>(6).as_str()),
                completion_step_count: count_json_array(r.get::<_, String>(7).as_str()),
                similarity: rank.min(1.0),
                full_json: None,
            }
        })
        .collect())
}

/// Find ground truth reference workflows from PG using keyword overlap.
pub async fn find_gt_reference_workflows_pg(
    pg: &Arc<PgDb>,
    description: &str,
    limit: usize,
) -> Result<Vec<SimilarWorkflow>, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let search_terms = extract_search_terms(description);
    let lim = limit as i64;

    // For GT workflows, fetch all ground_truth workflows and score by keyword overlap
    let rows = conn
        .query(
            r#"SELECT id, name, COALESCE(description, '') as description, category,
                  setup_steps, verification_steps, agentic_steps, completion_steps
           FROM unified_workflows
           WHERE category = 'ground_truth'
           ORDER BY updated_at DESC
           LIMIT $1"#,
            &[&lim],
        )
        .await
        .map_err(|e| format!("PG find_gt_reference_workflows: {}", e))?;

    let mut results: Vec<SimilarWorkflow> = rows.iter().map(|r| {
        let name: String = r.get(1);
        let desc: String = r.get(2);
        let similarity = keyword_overlap_score(&search_terms, &name, &desc);

        // Build full JSON for GT workflows
        let full_json = serde_json::json!({
            "id": r.get::<_, String>(0),
            "name": &name,
            "description": &desc,
            "category": r.get::<_, String>(3),
            "setup_steps": serde_json::from_str::<serde_json::Value>(r.get::<_, String>(4).as_str()).unwrap_or_default(),
            "verification_steps": serde_json::from_str::<serde_json::Value>(r.get::<_, String>(5).as_str()).unwrap_or_default(),
            "agentic_steps": serde_json::from_str::<serde_json::Value>(r.get::<_, String>(6).as_str()).unwrap_or_default(),
            "completion_steps": serde_json::from_str::<serde_json::Value>(r.get::<_, String>(7).as_str()).unwrap_or_default(),
        });

        SimilarWorkflow {
            id: r.get(0),
            name,
            description: desc,
            category: r.get(3),
            setup_step_count: count_json_array(r.get::<_, String>(4).as_str()),
            verification_step_count: count_json_array(r.get::<_, String>(5).as_str()),
            agentic_step_count: count_json_array(r.get::<_, String>(6).as_str()),
            completion_step_count: count_json_array(r.get::<_, String>(7).as_str()),
            similarity,
            full_json: Some(serde_json::to_string_pretty(&full_json).unwrap_or_default()),
        }
    }).collect();

    // Sort by similarity descending
    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(results)
}

/// Extract meaningful search terms from a description for full-text search.
fn extract_search_terms(description: &str) -> Vec<String> {
    let stop_words = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "and", "or", "but", "in", "on",
        "at", "to", "for", "of", "with", "that", "this", "it", "from", "by", "as", "not", "do",
        "does", "has", "have", "had", "will", "would", "could", "should", "can", "may", "might",
        "must", "shall", "need", "want",
    ];

    description
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 2 && !stop_words.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .into_iter()
        .take(10)
        .collect()
}

/// Compute keyword overlap score between search terms and text.
fn keyword_overlap_score(terms: &[String], name: &str, description: &str) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let combined = format!("{} {}", name, description).to_lowercase();
    let matches = terms
        .iter()
        .filter(|t| combined.contains(t.as_str()))
        .count();
    (matches as f32) / (terms.len() as f32)
}

/// Count elements in a JSON array string.
fn count_json_array(json_str: &str) -> usize {
    serde_json::from_str::<Vec<serde_json::Value>>(json_str)
        .map(|v| v.len())
        .unwrap_or(0)
}
