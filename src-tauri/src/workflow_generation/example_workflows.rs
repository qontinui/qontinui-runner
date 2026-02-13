//! Example Workflows via RAG
//!
//! Retrieves example workflows from the user's own successfully-run AI-generated
//! workflows for use as few-shot examples in the generation prompt.
//! The example library starts empty and grows organically as workflows succeed.

use rusqlite::Connection;
use serde::Serialize;
use tracing::{debug, error, info, warn};

/// Reference to a workflow used as an example in the generation prompt.
#[derive(Debug, Clone, Serialize)]
pub struct ExampleWorkflowRef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub workflow_json: String,
    pub similarity: f32,
}

/// Find relevant example workflows using cosine similarity when embeddings
/// are available, falling back to recency-based retrieval.
///
/// Returns up to `limit` examples sorted by relevance.
pub fn find_relevant_examples(
    conn: &Connection,
    query_embedding: Option<&[f32]>,
    category: Option<&str>,
    limit: usize,
) -> Vec<ExampleWorkflowRef> {
    if let Some(embedding) = query_embedding {
        match find_by_embedding(conn, embedding, category, limit) {
            Ok(results) if !results.is_empty() => return results,
            Ok(_) => debug!("No embedding-matched examples found, falling back to recency"),
            Err(e) => debug!("Embedding search failed, falling back to recency: {}", e),
        }
    }

    // Fallback: most recent active examples, optionally filtered by category
    match find_by_recency(conn, category, limit) {
        Ok(results) => results,
        Err(e) => {
            warn!("Failed to retrieve example workflows: {}", e);
            Vec::new()
        }
    }
}

type EmbeddingRow = (String, String, String, String, String, String, String, Vec<u8>);

fn map_embedding_row(row: &rusqlite::Row) -> rusqlite::Result<EmbeddingRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn find_by_embedding(
    conn: &Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<ExampleWorkflowRef>, String> {
    let query = if category.is_some() {
        "SELECT id, name, description, category, setup_steps, verification_steps, agentic_steps, \
         description_embedding FROM unified_workflows \
         WHERE example_status = 'active' AND description_embedding IS NOT NULL AND category = ?1 \
         ORDER BY updated_at DESC LIMIT 50"
    } else {
        "SELECT id, name, description, category, setup_steps, verification_steps, agentic_steps, \
         description_embedding FROM unified_workflows \
         WHERE example_status = 'active' AND description_embedding IS NOT NULL \
         ORDER BY updated_at DESC LIMIT 50"
    };

    let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;

    let rows: Vec<EmbeddingRow> = if let Some(cat) = category {
        stmt.query_map([cat], map_embedding_row)
    } else {
        stmt.query_map([], map_embedding_row)
    }
    .map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let mut scored: Vec<ExampleWorkflowRef> = rows
        .into_iter()
        .filter_map(
            |(id, name, description, category, setup, verification, agentic, embedding_blob)| {
                let embedding = blob_to_f32_vec(&embedding_blob)?;
                let similarity = cosine_similarity(query_embedding, &embedding);
                let workflow_json = reconstruct_workflow_json(&name, &description, &setup, &verification, &agentic);
                Some(ExampleWorkflowRef {
                    id,
                    name,
                    description,
                    category,
                    workflow_json,
                    similarity,
                })
            },
        )
        .collect();

    // Sort by similarity descending
    scored.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

type RecencyRow = (String, String, String, String, String, String, String);

fn map_recency_row(row: &rusqlite::Row) -> rusqlite::Result<RecencyRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn find_by_recency(
    conn: &Connection,
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<ExampleWorkflowRef>, String> {
    let query = if category.is_some() {
        "SELECT id, name, description, category, setup_steps, verification_steps, agentic_steps \
         FROM unified_workflows \
         WHERE example_status = 'active' AND category = ?1 \
         ORDER BY updated_at DESC LIMIT ?2"
    } else {
        "SELECT id, name, description, category, setup_steps, verification_steps, agentic_steps \
         FROM unified_workflows \
         WHERE example_status = 'active' \
         ORDER BY updated_at DESC LIMIT ?1"
    };

    let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;

    let rows: Vec<RecencyRow> = if let Some(cat) = category {
        stmt.query_map(rusqlite::params![cat, limit as i64], map_recency_row)
    } else {
        stmt.query_map(rusqlite::params![limit as i64], map_recency_row)
    }
    .map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let results = rows
        .into_iter()
        .map(
            |(id, name, description, category, setup, verification, agentic)| {
                let workflow_json = reconstruct_workflow_json(&name, &description, &setup, &verification, &agentic);
                ExampleWorkflowRef {
                    id,
                    name,
                    description,
                    category,
                    workflow_json,
                    similarity: 0.0,
                }
            },
        )
        .collect();

    Ok(results)
}

/// Format example workflows for inclusion in the AI prompt.
pub fn format_examples_for_prompt(examples: &[ExampleWorkflowRef], max_count: usize) -> String {
    if examples.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for (i, example) in examples.iter().take(max_count).enumerate() {
        output.push_str(&format!(
            "### Example {}: {}\nDescription: {}\n```json\n{}\n```\n\n",
            i + 1,
            example.name,
            example.description,
            example.workflow_json
        ));
    }
    output
}

/// Promote a workflow to the example library (set example_status = 'active').
pub fn promote_workflow_to_example(conn: &Connection, workflow_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE unified_workflows SET example_status = 'active' WHERE id = ?1",
        [workflow_id],
    )
    .map_err(|e| format!("Failed to promote workflow to example: {}", e))?;
    info!("Promoted workflow '{}' to example library", workflow_id);
    Ok(())
}

/// Exclude a workflow from ever being added to the example library.
pub fn exclude_workflow_from_examples(conn: &Connection, workflow_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE unified_workflows SET example_status = 'excluded' WHERE id = ?1",
        [workflow_id],
    )
    .map_err(|e| format!("Failed to exclude workflow from examples: {}", e))?;
    info!("Excluded workflow '{}' from example library", workflow_id);
    Ok(())
}

/// Remove a workflow from the example library (back to pending, allows re-add on next success).
pub fn remove_workflow_from_examples(conn: &Connection, workflow_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE unified_workflows SET example_status = 'pending' WHERE id = ?1",
        [workflow_id],
    )
    .map_err(|e| format!("Failed to remove workflow from examples: {}", e))?;
    info!(
        "Removed workflow '{}' from example library (back to pending)",
        workflow_id
    );
    Ok(())
}

/// Attempt to promote a workflow after a successful task run.
///
/// Gate conditions:
/// - Workflow was AI-generated (generated_by_task_run_id IS NOT NULL)
/// - example_status is 'pending' (not already active or excluded)
/// - category is not 'meta' (not a meta-generation workflow)
///
/// This is fire-and-forget — errors are logged but never propagated.
pub fn try_promote_on_success(conn: &Connection, workflow_id: &str) {
    let result: Result<(), String> = (|| {
        let row: Option<(Option<String>, String, String)> = conn
            .query_row(
                "SELECT generated_by_task_run_id, COALESCE(example_status, 'pending'), category \
                 FROM unified_workflows WHERE id = ?1",
                [workflow_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map(Some)
            .unwrap_or(None);

        let Some((generated_by, example_status, category)) = row else {
            debug!("Workflow '{}' not found for example promotion", workflow_id);
            return Ok(());
        };

        // Gate: must be AI-generated
        if generated_by.is_none() {
            return Ok(());
        }

        // Gate: must be pending
        if example_status != "pending" {
            return Ok(());
        }

        // Gate: must not be meta-workflow
        if category == "meta" {
            return Ok(());
        }

        promote_workflow_to_example(conn, workflow_id)?;
        info!(
            "Promoted workflow '{}' to example library after first successful run",
            workflow_id
        );
        Ok(())
    })();

    if let Err(e) = result {
        error!(
            "Failed to promote workflow '{}' to example library: {}",
            workflow_id, e
        );
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn reconstruct_workflow_json(
    name: &str,
    description: &str,
    setup_steps: &str,
    verification_steps: &str,
    agentic_steps: &str,
) -> String {
    // Build a compact representation focused on the steps structure
    format!(
        r#"{{"name":"{}","description":"{}","setup_steps":{},"verification_steps":{},"agentic_steps":{}}}"#,
        name.replace('"', "\\\""),
        description.replace('"', "\\\""),
        setup_steps,
        verification_steps,
        agentic_steps,
    )
}

fn blob_to_f32_vec(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c)).abs() < 0.001);
    }

    #[test]
    fn test_blob_to_f32_vec() {
        let val: f32 = 1.5;
        let bytes = val.to_le_bytes();
        let blob: Vec<u8> = bytes.to_vec();
        let result = blob_to_f32_vec(&blob).unwrap();
        assert_eq!(result.len(), 1);
        assert!((result[0] - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_blob_invalid_length() {
        assert!(blob_to_f32_vec(&[1, 2, 3]).is_none());
    }

    #[test]
    fn test_format_examples_empty() {
        assert!(format_examples_for_prompt(&[], 3).is_empty());
    }

    #[test]
    fn test_format_examples_single() {
        let examples = vec![ExampleWorkflowRef {
            id: "test-id".to_string(),
            name: "Test Workflow".to_string(),
            description: "A test".to_string(),
            category: "testing".to_string(),
            workflow_json: r#"{"name":"test"}"#.to_string(),
            similarity: 0.9,
        }];
        let output = format_examples_for_prompt(&examples, 3);
        assert!(output.contains("### Example 1: Test Workflow"));
        assert!(output.contains("Description: A test"));
        assert!(output.contains(r#"{"name":"test"}"#));
    }

    #[test]
    fn test_reconstruct_workflow_json() {
        let json = reconstruct_workflow_json(
            "My Workflow",
            "Does stuff",
            "[]",
            "[{\"type\":\"check\"}]",
            "[{\"type\":\"prompt\"}]",
        );
        assert!(json.contains("My Workflow"));
        assert!(json.contains("\"type\":\"check\""));
    }
}
