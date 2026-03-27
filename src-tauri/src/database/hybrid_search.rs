//! Hybrid retrieval service: SQL filter → vector re-rank.
//!
//! Combines structured SQL filtering (status, category, timestamps) with
//! semantic vector similarity (cosine distance on MiniLM embeddings) to
//! produce ranked search results across runner tables.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::embeddings::{blob_to_vector, cosine_similarity};

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

// ============================================================================
// Table-specific search functions
// ============================================================================

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

/// Search task_run_findings using hybrid SQL + vector approach.
pub fn hybrid_search_findings(
    conn: &Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    status: Option<&str>,
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<FindingResult>>, String> {
    let candidate_limit = config.limit * 3; // Fetch 3x for re-ranking
    let mut sql = String::from(
        "SELECT id, task_run_id, category, severity, title, description, status, \
         file_path, detected_at, description_embedding \
         FROM task_run_findings WHERE description_embedding IS NOT NULL",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(cat) = category {
        sql.push_str(&format!(" AND category = ?{}", param_idx));
        param_values.push(Box::new(cat.to_string()));
        param_idx += 1;
    }
    if let Some(st) = status {
        sql.push_str(&format!(" AND status = ?{}", param_idx));
        param_values.push(Box::new(st.to_string()));
        param_idx += 1;
    }
    let _ = param_idx;

    sql.push_str(&format!(
        " ORDER BY detected_at DESC LIMIT {}",
        candidate_limit
    ));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare findings search: {}", e))?;

    let candidates: Vec<(FindingResult, Option<Vec<f32>>)> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let embedding_blob: Option<Vec<u8>> = row.get(9)?;
            Ok((
                FindingResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    category: row.get(2)?,
                    severity: row.get(3)?,
                    title: row.get(4)?,
                    description: row.get(5)?,
                    status: row.get(6)?,
                    file_path: row.get(7)?,
                    detected_at: row.get(8)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query findings: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
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

/// Search task_runs using hybrid SQL + vector approach.
pub fn hybrid_search_task_runs(
    conn: &Connection,
    query_embedding: &[f32],
    status: Option<&str>,
    category: Option<&str>,
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<TaskRunResult>>, String> {
    let candidate_limit = config.limit * 3;
    let mut sql = String::from(
        "SELECT id, task_name, status, workflow_name, summary, created_at, prompt_embedding \
         FROM task_runs WHERE prompt_embedding IS NOT NULL",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(st) = status {
        sql.push_str(&format!(" AND status = ?{}", param_idx));
        param_values.push(Box::new(st.to_string()));
        param_idx += 1;
    }
    if let Some(cat) = category {
        sql.push_str(&format!(" AND task_type = ?{}", param_idx));
        param_values.push(Box::new(cat.to_string()));
        param_idx += 1;
    }
    let _ = param_idx;

    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT {}",
        candidate_limit
    ));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare task_runs search: {}", e))?;

    let candidates: Vec<(TaskRunResult, Option<Vec<f32>>)> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let embedding_blob: Option<Vec<u8>> = row.get(6)?;
            Ok((
                TaskRunResult {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    status: row.get(2)?,
                    workflow_name: row.get(3)?,
                    summary: row.get(4)?,
                    created_at: row.get(5)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query task_runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
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

/// Search error_events using FTS5 pre-filter + vector re-rank.
pub fn hybrid_search_error_events(
    conn: &Connection,
    query_text: &str,
    query_embedding: &[f32],
    severity: Option<&str>,
    source: Option<&str>,
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<ErrorEventResult>>, String> {
    let candidate_limit = config.limit * 3;

    // Use FTS5 pre-filter for text matching, then vector re-rank
    let mut sql = String::from(
        "SELECT e.id, e.log_source_name, e.severity, e.message, e.error_type, \
         e.status, e.occurrence_count, e.last_seen_at, e.message_embedding \
         FROM error_events e \
         JOIN error_events_fts fts ON fts.rowid = e.id \
         WHERE error_events_fts MATCH ?1",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    // Escape special FTS5 characters in query text
    let fts_query = escape_fts5_query(query_text);
    param_values.push(Box::new(fts_query));
    let mut param_idx = 2;

    if let Some(sev) = severity {
        sql.push_str(&format!(" AND e.severity = ?{}", param_idx));
        param_values.push(Box::new(sev.to_string()));
        param_idx += 1;
    }
    if let Some(src) = source {
        sql.push_str(&format!(" AND e.log_source_name = ?{}", param_idx));
        param_values.push(Box::new(src.to_string()));
        param_idx += 1;
    }
    let _ = param_idx;

    sql.push_str(&format!(
        " ORDER BY e.last_seen_at DESC LIMIT {}",
        candidate_limit
    ));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare error search: {}", e))?;

    let candidates: Vec<(ErrorEventResult, Option<Vec<f32>>)> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let embedding_blob: Option<Vec<u8>> = row.get(8)?;
            Ok((
                ErrorEventResult {
                    id: row.get(0)?,
                    log_source_name: row.get(1)?,
                    severity: row.get(2)?,
                    message: row.get(3)?,
                    error_type: row.get(4)?,
                    status: row.get(5)?,
                    occurrence_count: row.get(6)?,
                    last_seen_at: row.get(7)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query error events: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
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

/// Search task_knowledge using hybrid SQL + vector approach.
pub fn hybrid_search_knowledge(
    conn: &Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<KnowledgeResult>>, String> {
    let candidate_limit = config.limit * 3;
    let mut sql = String::from(
        "SELECT id, task_run_id, category, content, confidence, is_resolved, created_at, \
         content_embedding FROM task_knowledge WHERE content_embedding IS NOT NULL",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(cat) = category {
        sql.push_str(&format!(" AND category = ?{}", param_idx));
        param_values.push(Box::new(cat.to_string()));
        param_idx += 1;
    }
    let _ = param_idx;

    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT {}",
        candidate_limit
    ));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare knowledge search: {}", e))?;

    let candidates: Vec<(KnowledgeResult, Option<Vec<f32>>)> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let embedding_blob: Option<Vec<u8>> = row.get(7)?;
            Ok((
                KnowledgeResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    category: row.get(2)?,
                    content: row.get(3)?,
                    confidence: row.get(4)?,
                    is_resolved: row.get::<_, i32>(5)? != 0,
                    created_at: row.get(6)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query knowledge: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
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

/// Search unified_workflows using hybrid SQL + vector approach.
pub fn hybrid_search_workflows(
    conn: &Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<WorkflowResult>>, String> {
    let candidate_limit = config.limit * 3;
    let mut sql = String::from(
        "SELECT id, name, description, category, setup_steps, verification_steps, \
         agentic_steps, created_at, description_embedding \
         FROM unified_workflows WHERE description_embedding IS NOT NULL \
         AND category != 'meta'",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(cat) = category {
        sql.push_str(&format!(" AND category = ?{}", param_idx));
        param_values.push(Box::new(cat.to_string()));
        param_idx += 1;
    }
    let _ = param_idx;

    sql.push_str(&format!(
        " ORDER BY updated_at DESC LIMIT {}",
        candidate_limit
    ));

    let params_slice: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare workflow search: {}", e))?;

    let candidates: Vec<(WorkflowResult, Option<Vec<f32>>)> = stmt
        .query_map(params_slice.as_slice(), |row| {
            let setup_json: String = row.get(4)?;
            let verification_json: String = row.get(5)?;
            let agentic_json: String = row.get(6)?;
            let embedding_blob: Option<Vec<u8>> = row.get(8)?;

            let count_json_array = |s: &str| -> usize {
                serde_json::from_str::<Vec<serde_json::Value>>(s)
                    .map(|v| v.len())
                    .unwrap_or(0)
            };

            Ok((
                WorkflowResult {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    category: row.get(3)?,
                    setup_step_count: count_json_array(&setup_json),
                    verification_step_count: count_json_array(&verification_json),
                    agentic_step_count: count_json_array(&agentic_json),
                    created_at: row.get(7)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query workflows: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
}

// ============================================================================
// Universal fix search
// ============================================================================

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

/// Search universal reflection fixes using hybrid SQL + vector approach.
///
/// SQL pre-filter: scope='universal', status='applied'
/// Vector re-rank: cosine similarity between query_embedding and fix_description_embedding
pub fn hybrid_search_universal_fixes(
    conn: &Connection,
    query_embedding: &[f32],
    config: &HybridSearchConfig,
) -> Result<Vec<SearchResult<UniversalFixResult>>, String> {
    let candidate_limit = config.limit * 3;
    let sql = format!(
        "SELECT id, fix_type, fix_description, applicability_context, reuse_count, confidence, \
         created_at, fix_description_embedding \
         FROM reflection_fixes \
         WHERE reflection_scope = 'universal' AND status = 'applied' \
         AND fix_description_embedding IS NOT NULL \
         ORDER BY reuse_count DESC, created_at DESC \
         LIMIT {}",
        candidate_limit
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare universal fix search: {}", e))?;

    let candidates: Vec<(UniversalFixResult, Option<Vec<f32>>)> = stmt
        .query_map([], |row| {
            let embedding_blob: Option<Vec<u8>> = row.get(7)?;
            Ok((
                UniversalFixResult {
                    id: row.get(0)?,
                    fix_type: row.get(1)?,
                    fix_description: row.get(2)?,
                    applicability_context: row.get(3)?,
                    reuse_count: row.get(4)?,
                    confidence: row.get(5)?,
                    created_at: row.get(6)?,
                },
                embedding_blob.and_then(|b| blob_to_vector(&b)),
            ))
        })
        .map_err(|e| format!("Failed to query universal fixes: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rank_results(candidates, query_embedding, config))
}

// ============================================================================
// Re-ranking logic
// ============================================================================

/// Rank candidates using hybrid scoring (SQL position + vector similarity).
pub(crate) fn rank_results<T>(
    candidates: Vec<(T, Option<Vec<f32>>)>,
    query_embedding: &[f32],
    config: &HybridSearchConfig,
) -> Vec<SearchResult<T>> {
    let total = candidates.len() as f32;
    if total == 0.0 {
        return Vec::new();
    }

    let mut scored: Vec<SearchResult<T>> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(rank, (item, embedding))| {
            let similarity = embedding
                .as_ref()
                .map(|emb| cosine_similarity(query_embedding, emb))
                .unwrap_or(0.0);

            if similarity < config.min_similarity {
                return None;
            }

            // SQL rank score: 1.0 for first result, decreasing linearly
            let sql_rank_score = 1.0 - (rank as f32 / total);

            let hybrid_score =
                config.sql_weight * sql_rank_score + config.vector_weight * similarity;

            Some(SearchResult {
                item,
                similarity,
                hybrid_score,
            })
        })
        .collect();

    // Sort by hybrid score descending
    scored.sort_by(|a, b| {
        b.hybrid_score
            .partial_cmp(&a.hybrid_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to limit
    scored.truncate(config.limit);

    scored
}

/// Escape a query string for FTS5 MATCH syntax.
///
/// Wraps each word in quotes to prevent FTS5 syntax errors from special chars.
fn escape_fts5_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|word| {
            // Remove special FTS5 characters
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() {
                String::new()
            } else {
                format!("\"{}\"", cleaned)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_results_empty() {
        let candidates: Vec<(String, Option<Vec<f32>>)> = Vec::new();
        let query = vec![0.0f32; 384];
        let config = HybridSearchConfig::default();
        let results = rank_results(candidates, &query, &config);
        assert!(results.is_empty());
    }

    #[test]
    fn test_rank_results_ordering() {
        let query = vec![1.0, 0.0, 0.0];
        let candidates = vec![
            ("low_similarity".to_string(), Some(vec![0.0, 1.0, 0.0])),
            ("high_similarity".to_string(), Some(vec![0.9, 0.1, 0.0])),
            ("no_embedding".to_string(), None),
        ];
        let config = HybridSearchConfig {
            min_similarity: 0.0,
            ..Default::default()
        };

        let results = rank_results(candidates, &query, &config);
        // High similarity should rank first due to vector_weight = 0.7
        assert!(results.len() >= 2);
        assert!(results[0].similarity > results[1].similarity);
    }

    #[test]
    fn test_escape_fts5_query() {
        assert_eq!(escape_fts5_query("hello world"), "\"hello\" \"world\"");
        assert_eq!(escape_fts5_query("error: timeout"), "\"error\" \"timeout\"");
        assert_eq!(escape_fts5_query(""), "");
    }

    #[test]
    fn test_rank_results_min_similarity_filter() {
        let query = vec![1.0, 0.0, 0.0];
        let candidates = vec![
            ("orthogonal".to_string(), Some(vec![0.0, 1.0, 0.0])),
            ("similar".to_string(), Some(vec![0.8, 0.2, 0.0])),
        ];
        let config = HybridSearchConfig {
            min_similarity: 0.5,
            ..Default::default()
        };

        let results = rank_results(candidates, &query, &config);
        // Orthogonal vector (similarity ~0) should be filtered out
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item, "similar");
    }

    #[test]
    fn test_rank_results_no_embeddings() {
        let query = vec![1.0, 0.0, 0.0];
        let candidates = vec![("no_emb1".to_string(), None), ("no_emb2".to_string(), None)];
        let config = HybridSearchConfig {
            min_similarity: 0.0,
            ..Default::default()
        };

        let results = rank_results(candidates, &query, &config);
        // Items without embeddings should still appear (with 0 similarity)
        // but only if min_similarity is 0
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.similarity, 0.0);
        }
    }

    #[test]
    fn test_hybrid_search_config_defaults() {
        let config = HybridSearchConfig::default();
        assert_eq!(config.sql_weight, 0.3);
        assert_eq!(config.vector_weight, 0.7);
        assert_eq!(config.limit, 20);
        assert_eq!(config.min_similarity, 0.5);
    }

    #[test]
    fn test_escape_fts5_special_chars() {
        // Special characters (quotes, etc.) are stripped, keeping alphanumeric + _ + -
        assert_eq!(escape_fts5_query("foo\"bar"), "\"foobar\"");
        // Hyphens and underscores are preserved
        assert_eq!(escape_fts5_query("a-b_c"), "\"a-b_c\"");
    }
}
