//! Hybrid retrieval service: SQL filter → vector re-rank.
//!
//! Combines structured SQL filtering (status, category, timestamps) with
//! semantic vector similarity (cosine distance on MiniLM embeddings) to
//! produce ranked search results across runner tables.
use serde::{Deserialize, Serialize};

/// Configuration for hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchConfig {
    /// Weight for SQL rank position (0.0 to 1.0).
    pub sql_weight: f32,
    /// Weight for vector similarity (0.0 to 1.0).
    pub vector_weight: f32,
    /// Maximum results to return.
    pub limit: usize,
    /// Minimum cosine similarity threshold (skip results below this).
    pub min_similarity: f32,
}

/// A single search result with scoring metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult<T> {
    /// The matched item.
    pub item: T,
    /// Cosine similarity score (0.0 to 1.0).
    pub similarity: f32,
    /// Hybrid score (weighted combination of SQL rank and similarity).
    pub hybrid_score: f32,
}

/// A finding result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingResult {
    pub id: String,
    pub task_run_id: String,
    pub category: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub file_path: Option<String>,
    pub detected_at: String,
}

/// A task run result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunResult {
    pub id: String,
    pub task_name: String,
    pub status: String,
    pub workflow_name: Option<String>,
    pub summary: Option<String>,
    pub created_at: String,
}

/// An error event result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEventResult {
    pub id: i64,
    pub log_source_name: String,
    pub severity: String,
    pub message: String,
    pub error_type: Option<String>,
    pub status: String,
    pub occurrence_count: i64,
    pub last_seen_at: String,
}

/// A task knowledge result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub id: String,
    pub task_run_id: String,
    pub category: String,
    pub content: String,
    pub confidence: String,
    pub is_resolved: bool,
    pub created_at: String,
}

/// A workflow result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub setup_step_count: usize,
    pub verification_step_count: usize,
    pub agentic_step_count: usize,
    pub created_at: String,
}

/// A universal fix result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniversalFixResult {
    pub id: String,
    pub fix_type: String,
    pub fix_description: String,
    pub applicability_context: Option<String>,
    pub reuse_count: i32,
    pub confidence: String,
    pub created_at: String,
}

/// Rank candidate (item, optional embedding) pairs by cosine similarity to a
/// query embedding. Candidates without embeddings are dropped. Results below
/// `config.min_similarity` are filtered out, then sorted descending by score
/// and truncated to `config.limit`.
///
/// Note: there is no separate SQL-rank signal here (the SQL filter has already
/// been applied by the caller), so `hybrid_score == similarity`. The
/// `sql_weight`/`vector_weight` fields are reserved for future use.
pub fn rank_results<T>(
    candidates: Vec<(T, Option<Vec<f32>>)>,
    query_embedding: &[f32],
    config: &HybridSearchConfig,
) -> Vec<SearchResult<T>> {
    let mut scored: Vec<SearchResult<T>> = candidates
        .into_iter()
        .filter_map(|(item, emb)| {
            let emb = emb?;
            if emb.len() != query_embedding.len() {
                return None;
            }
            let similarity =
                crate::database::embeddings::cosine_similarity(query_embedding, &emb);
            if similarity < config.min_similarity {
                return None;
            }
            Some(SearchResult {
                item,
                similarity,
                hybrid_score: similarity,
            })
        })
        .collect();

    scored.sort_by(|a, b| {
        b.hybrid_score
            .partial_cmp(&a.hybrid_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(config.limit);
    scored
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            sql_weight: 0.3,
            vector_weight: 0.7,
            limit: 20,
            min_similarity: 0.5,
        }
    }
}
