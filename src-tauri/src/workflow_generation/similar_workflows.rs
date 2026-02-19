//! Similar workflow retrieval for workflow generation context.
//!
//! Finds semantically similar existing workflows to use as reference examples
//! when generating new workflows. Includes special handling for ground truth
//! workflows that provide full JSON examples.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::database::embeddings::{blob_to_vector, cosine_similarity};

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
    let mut sql = String::from(
        "SELECT id, name, description, category, setup_steps, verification_steps, \
         agentic_steps, completion_steps, description_embedding \
         FROM unified_workflows \
         WHERE description_embedding IS NOT NULL AND category != 'meta'",
    );

    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(cat) = category {
        sql.push_str(" AND category = ?1");
        param_values.push(Box::new(cat.to_string()));
    }

    // Fetch a larger candidate set for re-ranking
    sql.push_str(&format!(" ORDER BY updated_at DESC LIMIT {}", limit * 5));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare similar workflows query: {}", e))?;

    let count_steps = |json: &str| -> usize {
        serde_json::from_str::<Vec<serde_json::Value>>(json)
            .map(|v| v.len())
            .unwrap_or(0)
    };

    let mut candidates: Vec<SimilarWorkflow> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let setup_json: String = row.get(4)?;
            let verification_json: String = row.get(5)?;
            let agentic_json: String = row.get(6)?;
            let completion_json: String = row.get(7)?;
            let embedding_blob: Option<Vec<u8>> = row.get(8)?;

            let similarity = embedding_blob
                .and_then(|b| blob_to_vector(&b))
                .map(|emb| cosine_similarity(query_embedding, &emb))
                .unwrap_or(0.0);

            let category: String = row.get(3)?;

            Ok(SimilarWorkflow {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                category,
                setup_step_count: count_steps(&setup_json),
                verification_step_count: count_steps(&verification_json),
                agentic_step_count: count_steps(&agentic_json),
                completion_step_count: count_steps(&completion_json),
                similarity,
                full_json: None,
            })
        })
        .map_err(|e| format!("Failed to query similar workflows: {}", e))?
        .filter_map(|r| r.ok())
        .filter(|w| w.similarity > 0.3) // Minimum similarity threshold
        .collect();

    // Sort by similarity descending
    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates.truncate(limit);

    Ok(candidates)
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
    let sql = "SELECT id, name, description, category, setup_steps, verification_steps, \
               agentic_steps, completion_steps, description_embedding \
               FROM unified_workflows \
               WHERE category = 'ground_truth' \
               ORDER BY updated_at DESC LIMIT 50";

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare GT workflows query: {}", e))?;

    let count_steps = |json: &str| -> usize {
        serde_json::from_str::<Vec<serde_json::Value>>(json)
            .map(|v| v.len())
            .unwrap_or(0)
    };

    // Extract keywords from the query description (lowercased, split on non-alphanumeric)
    let query_lower = description.to_lowercase();
    let query_words: Vec<&str> = query_lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() >= 3) // Skip very short words
        .collect();

    let mut candidates: Vec<SimilarWorkflow> = stmt
        .query_map([], |row| {
            let setup_json: String = row.get(4)?;
            let verification_json: String = row.get(5)?;
            let agentic_json: String = row.get(6)?;
            let completion_json: String = row.get(7)?;
            let embedding_blob: Option<Vec<u8>> = row.get(8)?;

            // Keyword-based similarity: count how many query words appear in GT description
            let gt_desc: String = row.get(2)?;
            let gt_desc_lower = gt_desc.to_lowercase();
            let matching_words = query_words
                .iter()
                .filter(|w| gt_desc_lower.contains(**w))
                .count();
            let keyword_score = if query_words.is_empty() {
                0.0
            } else {
                matching_words as f32 / query_words.len() as f32
            };

            // Embedding-based similarity (if available)
            let embedding_score = query_embedding
                .and_then(|qe| {
                    embedding_blob
                        .and_then(|b| blob_to_vector(&b))
                        .map(|emb| cosine_similarity(qe, &emb))
                })
                .unwrap_or(0.0);

            // Combined score: keyword is primary, embedding boosts
            let similarity = if embedding_score > 0.0 {
                keyword_score * 0.6 + embedding_score * 0.4
            } else {
                keyword_score
            };

            // Build full workflow JSON for GT examples
            let full_json = serde_json::json!({
                "setup_steps": serde_json::from_str::<serde_json::Value>(&setup_json).unwrap_or_default(),
                "verification_steps": serde_json::from_str::<serde_json::Value>(&verification_json).unwrap_or_default(),
                "agentic_steps": serde_json::from_str::<serde_json::Value>(&agentic_json).unwrap_or_default(),
                "completion_steps": serde_json::from_str::<serde_json::Value>(&completion_json).unwrap_or_default(),
            });

            Ok(SimilarWorkflow {
                id: row.get(0)?,
                name: row.get(1)?,
                description: gt_desc,
                category: row.get(3)?,
                setup_step_count: count_steps(&setup_json),
                verification_step_count: count_steps(&verification_json),
                agentic_step_count: count_steps(&agentic_json),
                completion_step_count: count_steps(&completion_json),
                similarity,
                full_json: Some(serde_json::to_string_pretty(&full_json).unwrap_or_default()),
            })
        })
        .map_err(|e| format!("Failed to query GT workflows: {}", e))?
        .filter_map(|r| r.ok())
        .filter(|w| w.similarity > 0.25) // Lower threshold since keyword matching
        .collect();

    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates.truncate(limit);

    Ok(candidates)
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
