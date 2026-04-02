//! Example Workflows via RAG
//!
//! Retrieves example workflows from the user's own successfully-run AI-generated
//! workflows for use as few-shot examples in the generation prompt.
//! The example library starts empty and grows organically as workflows succeed.

use serde::Serialize;

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
    query_embedding: Option<&[f32]>,
    category: Option<&str>,
    limit: usize,
) -> Vec<ExampleWorkflowRef> {
    Vec::new()
}

type EmbeddingRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<u8>,
);

// map_embedding_row removed (SQLite dead code)

fn find_by_embedding(
    query_embedding: &[f32],
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<ExampleWorkflowRef>, String> {
    Err("SQLite removed".to_string())
}

type RecencyRow = (String, String, String, String, String, String, String);

// map_recency_row removed (SQLite dead code)

fn find_by_recency(
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<ExampleWorkflowRef>, String> {
    Err("SQLite removed".to_string())
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
pub fn promote_workflow_to_example(workflow_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Exclude a workflow from ever being added to the example library.
pub fn exclude_workflow_from_examples(workflow_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Remove a workflow from the example library (back to pending, allows re-add on next success).
pub fn remove_workflow_from_examples(workflow_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Attempt to promote a workflow after a successful task run.
///
/// Gate conditions:
/// - Workflow was AI-generated (generated_by_task_run_id IS NOT NULL)
/// - example_status is 'pending' (not already active or excluded)
/// - category is not 'meta' (not a meta-generation workflow)
///
/// This is fire-and-forget — errors are logged but never propagated.
pub fn try_promote_on_success(workflow_id: &str) {
    // SQLite removed - no-op
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
    if !blob.len().is_multiple_of(4) {
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
