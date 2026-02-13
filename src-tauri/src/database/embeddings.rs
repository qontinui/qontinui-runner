//! Vector embedding utilities for hybrid RAG search.
//!
//! Provides conversion between f32 vectors and SQLite BLOB format,
//! cosine similarity computation, and per-table store/retrieve helpers.

use rusqlite::{params, Connection};
use tracing::warn;

/// Embedding dimension for MiniLM-L6-v2 model (384 dimensions).
pub const EMBEDDING_DIM: usize = 384;

/// Expected BLOB size in bytes (384 * 4 bytes per f32 = 1536).
pub const EMBEDDING_BLOB_SIZE: usize = EMBEDDING_DIM * std::mem::size_of::<f32>();

/// Convert a f32 vector to a BLOB (little-endian byte representation).
pub fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(std::mem::size_of_val(vector));
    for &val in vector {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Convert a BLOB back to a f32 vector.
///
/// Returns `None` if the blob size is not a multiple of 4 bytes.
pub fn blob_to_vector(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % std::mem::size_of::<f32>() != 0 {
        return None;
    }
    let count = blob.len() / std::mem::size_of::<f32>();
    let mut vector = Vec::with_capacity(count);
    for chunk in blob.chunks_exact(std::mem::size_of::<f32>()) {
        vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(vector)
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [-1.0, 1.0] where 1.0 means identical direction.
/// Returns 0.0 if either vector has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vectors must have same dimension");

    let mut dot = 0.0f32;
    let mut mag_a = 0.0f32;
    let mut mag_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        mag_a += a[i] * a[i];
        mag_b += b[i] * b[i];
    }

    let magnitude = (mag_a * mag_b).sqrt();
    if magnitude == 0.0 {
        return 0.0;
    }
    dot / magnitude
}

// ============================================================================
// Per-table store/retrieve helpers
// ============================================================================

/// Store an embedding for a task_run column.
pub fn store_task_run_embedding(
    conn: &Connection,
    task_run_id: &str,
    column: TaskRunEmbeddingColumn,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    let col_name = column.as_str();
    let sql = format!("UPDATE task_runs SET {} = ?1 WHERE id = ?2", col_name);
    conn.execute(&sql, params![blob, task_run_id])
        .map_err(|e| format!("Failed to store {} embedding: {}", col_name, e))?;
    Ok(())
}

/// Retrieve an embedding from a task_run column.
pub fn get_task_run_embedding(
    conn: &Connection,
    task_run_id: &str,
    column: TaskRunEmbeddingColumn,
) -> Result<Option<Vec<f32>>, String> {
    let col_name = column.as_str();
    let sql = format!("SELECT {} FROM task_runs WHERE id = ?1", col_name);
    let result: Option<Vec<u8>> = conn
        .query_row(&sql, params![task_run_id], |row| row.get(0))
        .map_err(|e| format!("Failed to get {} embedding: {}", col_name, e))?;
    Ok(result.and_then(|blob| blob_to_vector(&blob)))
}

/// Store an embedding for a finding column.
pub fn store_finding_embedding(
    conn: &Connection,
    finding_id: &str,
    column: FindingEmbeddingColumn,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    let col_name = column.as_str();
    let sql = format!(
        "UPDATE task_run_findings SET {} = ?1 WHERE id = ?2",
        col_name
    );
    conn.execute(&sql, params![blob, finding_id])
        .map_err(|e| format!("Failed to store {} embedding: {}", col_name, e))?;
    Ok(())
}

/// Store an embedding for a task_knowledge row.
pub fn store_knowledge_embedding(
    conn: &Connection,
    knowledge_id: &str,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    conn.execute(
        "UPDATE task_knowledge SET content_embedding = ?1 WHERE id = ?2",
        params![blob, knowledge_id],
    )
    .map_err(|e| format!("Failed to store knowledge embedding: {}", e))?;
    Ok(())
}

/// Store an embedding for a unified_workflow description.
pub fn store_workflow_embedding(
    conn: &Connection,
    workflow_id: &str,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    conn.execute(
        "UPDATE unified_workflows SET description_embedding = ?1 WHERE id = ?2",
        params![blob, workflow_id],
    )
    .map_err(|e| format!("Failed to store workflow embedding: {}", e))?;
    Ok(())
}

/// Store an embedding for a learning_outcome context.
pub fn store_learning_outcome_embedding(
    conn: &Connection,
    outcome_id: &str,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    conn.execute(
        "UPDATE learning_outcomes SET context_embedding = ?1 WHERE id = ?2",
        params![blob, outcome_id],
    )
    .map_err(|e| format!("Failed to store learning outcome embedding: {}", e))?;
    Ok(())
}

/// Store an embedding for a learning_pattern description.
pub fn store_learning_pattern_embedding(
    conn: &Connection,
    pattern_id: &str,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    conn.execute(
        "UPDATE learning_patterns SET description_embedding = ?1 WHERE id = ?2",
        params![blob, pattern_id],
    )
    .map_err(|e| format!("Failed to store learning pattern embedding: {}", e))?;
    Ok(())
}

/// Store an embedding for an error_event message.
pub fn store_error_event_embedding(
    conn: &Connection,
    event_id: i64,
    embedding: &[f32],
) -> Result<(), String> {
    let blob = vector_to_blob(embedding);
    conn.execute(
        "UPDATE error_events SET message_embedding = ?1 WHERE id = ?2",
        params![blob, event_id],
    )
    .map_err(|e| format!("Failed to store error event embedding: {}", e))?;
    Ok(())
}

/// Batch retrieve rows that need embeddings for a given table/column.
///
/// Returns (id, text) pairs for rows where the embedding column is NULL
/// and the text column is not NULL/empty.
pub fn get_rows_needing_embeddings(
    conn: &Connection,
    table: &str,
    id_column: &str,
    text_column: &str,
    embedding_column: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, String> {
    // Only allow known table/column names to prevent injection
    if !is_valid_embedding_table(table) {
        return Err(format!("Invalid table for embedding lookup: {}", table));
    }

    let sql = format!(
        "SELECT {id_col}, {text_col} FROM {table} \
         WHERE {emb_col} IS NULL AND {text_col} IS NOT NULL AND {text_col} != '' \
         ORDER BY rowid DESC LIMIT ?1",
        id_col = id_column,
        text_col = text_column,
        table = table,
        emb_col = embedding_column,
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare embedding query: {}", e))?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("Failed to query rows needing embeddings: {}", e))?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(pair) => results.push(pair),
            Err(e) => warn!("Skipping row in embedding query: {}", e),
        }
    }
    Ok(results)
}

/// Validate that a table name is one of our known embedding tables.
fn is_valid_embedding_table(table: &str) -> bool {
    matches!(
        table,
        "task_runs"
            | "task_run_findings"
            | "task_knowledge"
            | "unified_workflows"
            | "learning_outcomes"
            | "learning_patterns"
            | "error_events"
    )
}

// ============================================================================
// Column enums
// ============================================================================

/// Embedding columns in the task_runs table.
#[derive(Debug, Clone, Copy)]
pub enum TaskRunEmbeddingColumn {
    Prompt,
    Summary,
}

impl TaskRunEmbeddingColumn {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prompt => "prompt_embedding",
            Self::Summary => "summary_embedding",
        }
    }
}

/// Embedding columns in the task_run_findings table.
#[derive(Debug, Clone, Copy)]
pub enum FindingEmbeddingColumn {
    Title,
    Description,
}

impl FindingEmbeddingColumn {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Title => "title_embedding",
            Self::Description => "description_embedding",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_blob_roundtrip() {
        let original: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();
        let blob = vector_to_blob(&original);
        assert_eq!(blob.len(), EMBEDDING_BLOB_SIZE);

        let recovered = blob_to_vector(&blob).unwrap();
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn test_blob_to_vector_invalid_size() {
        assert!(blob_to_vector(&[1, 2, 3]).is_none()); // not a multiple of 4
        assert!(blob_to_vector(&[]).unwrap().is_empty()); // empty is valid
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![0.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_high_dim() {
        // Test with actual 384-dim vectors
        let a: Vec<f32> = (0..384).map(|i| (i as f32 * 0.1).sin()).collect();
        let b = a.clone();
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-5, "Self-similarity should be 1.0");

        // Slightly perturbed vector should be very similar
        let mut c = a.clone();
        c[0] += 0.01;
        let sim2 = cosine_similarity(&a, &c);
        assert!(sim2 > 0.99, "Perturbed vector should be very similar");
    }

    #[test]
    fn test_vector_blob_single_value() {
        let v = vec![3.14f32];
        let blob = vector_to_blob(&v);
        assert_eq!(blob.len(), 4);
        let recovered = blob_to_vector(&blob).unwrap();
        assert_eq!(recovered, v);
    }

    #[test]
    fn test_vector_blob_negative_values() {
        let v = vec![-1.0f32, -0.5, 0.0, 0.5, 1.0];
        let blob = vector_to_blob(&v);
        let recovered = blob_to_vector(&blob).unwrap();
        assert_eq!(recovered, v);
    }
}
