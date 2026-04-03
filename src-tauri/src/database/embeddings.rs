//! Vector embedding utilities for hybrid RAG search.
//!
//! Provides conversion between f32 vectors and BLOB format,
//! cosine similarity computation.

/// Embedding dimension for MiniLM-L6-v2 model (384 dimensions).
pub const EMBEDDING_DIM: usize = 384;

/// Expected BLOB size in bytes (384 * 4 bytes per f32 = 1536).
pub const EMBEDDING_BLOB_SIZE: usize = EMBEDDING_DIM * std::mem::size_of::<f32>();

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

/// Store a knowledge embedding (stub -- SQLite removed).
pub fn store_knowledge_embedding(_knowledge_id: &str, _embedding: &[f32]) -> Result<(), String> {
    Err("SQLite removed".to_string())
}
