//! Unified memory query with Reciprocal Rank Fusion.
//!
//! Queries all memory stores in parallel and fuses results using RRF scoring.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

use crate::database::pg::PgDb;
use crate::reflection::graph_engine::KnowledgeGraph;

// =============================================================================
// Types
// =============================================================================

/// A single result from any memory store, normalized for fusion.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryResult {
    /// Unique identifier within its source.
    pub id: String,
    /// Which memory store produced this result.
    pub source: MemorySource,
    /// Display title.
    pub title: String,
    /// Content snippet (truncated to 500 chars).
    pub snippet: String,
    /// Per-source relevance score (0.0-1.0, normalized).
    pub source_score: f64,
    /// Fused score after RRF (set by fusion step).
    pub fused_score: f64,
    /// When this memory was created/captured.
    pub timestamp: Option<DateTime<Utc>>,
    /// Memory type classification (e.g. "bugfix", "pattern", "error_event", "capture").
    pub memory_type: String,
    /// Which retrieval strategies found this result.
    pub found_by: Vec<RetrievalStrategy>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Observation,
    ActivityTimeline,
    TaskKnowledge,
    Finding,
    Fix,
    Error,
    Rule,
    GraphNode,
    Workflow,
    UiElement,
}

impl MemorySource {
    /// Parse a comma-separated list of source names.
    pub fn parse_list(s: &str) -> Vec<Self> {
        s.split(',')
            .filter_map(|name| match name.trim() {
                "observation" => Some(Self::Observation),
                "activity_timeline" | "timeline" => Some(Self::ActivityTimeline),
                "task_knowledge" | "knowledge" => Some(Self::TaskKnowledge),
                "finding" => Some(Self::Finding),
                "fix" => Some(Self::Fix),
                "error" => Some(Self::Error),
                "rule" => Some(Self::Rule),
                "graph_node" | "graph" => Some(Self::GraphNode),
                "workflow" => Some(Self::Workflow),
                "ui_element" => Some(Self::UiElement),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStrategy {
    FullTextSearch,
    EmbeddingSimilarity,
    GraphTraversal,
    CategoryMatch,
}

/// Reasoning depth for tiered memory queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningLevel {
    /// Raw RRF-fused results, no enrichment.
    Minimal,
    /// + graph neighborhood enrichment for top results.
    Low,
    /// + causal context from graph traversal.
    Medium,
    /// + LLM synthesis of results into a coherent answer.
    High,
    /// + full research report via LLM.
    Max,
}

impl Default for ReasoningLevel {
    fn default() -> Self {
        Self::Minimal
    }
}

impl ReasoningLevel {
    pub fn from_str_opt(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            _ => Self::Minimal,
        }
    }
}

/// Response from a reasoned memory query (higher tiers).
#[derive(Debug, Clone, Serialize)]
pub struct ReasonedMemoryResponse {
    /// The fused memory results.
    pub results: Vec<MemoryResult>,
    /// Reasoning level that was applied.
    pub reasoning_level: ReasoningLevel,
    /// Graph neighborhood context for top results (Low+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighborhood_context: Option<Vec<NeighborhoodSnippet>>,
    /// Causal chains traced from top results (Medium+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_context: Option<Vec<CausalChain>>,
    /// LLM-generated synthesis of the results (High+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<String>,
}

/// A snippet of graph neighborhood context for a result.
#[derive(Debug, Clone, Serialize)]
pub struct NeighborhoodSnippet {
    pub result_id: String,
    pub source: MemorySource,
    pub neighbors: Vec<NeighborSummary>,
}

/// Summary of a graph neighbor.
#[derive(Debug, Clone, Serialize)]
pub struct NeighborSummary {
    pub label: String,
    pub kind: String,
    pub edge_label: String,
    pub direction: String,
    pub distance: u32,
}

/// A causal chain traced from a result node.
#[derive(Debug, Clone, Serialize)]
pub struct CausalChain {
    pub result_id: String,
    pub source: MemorySource,
    pub root_causes: Vec<String>,
    pub path_summary: String,
}

/// Parameters for a unified memory query.
#[derive(Debug, Clone, Deserialize)]
pub struct UnifiedMemoryQuery {
    /// The search query string.
    pub query: String,
    /// Maximum results to return (default 20).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Restrict to specific sources (None = all).
    pub sources: Option<Vec<MemorySource>>,
    /// Temporal filter: results after this time.
    pub from: Option<DateTime<Utc>>,
    /// Temporal filter: results before this time.
    pub to: Option<DateTime<Utc>>,
    /// Minimum fused_score threshold.
    pub min_score: Option<f64>,
    /// Reasoning depth (default: minimal).
    #[serde(default)]
    pub reasoning_level: ReasoningLevel,
}

fn default_limit() -> usize {
    20
}

// =============================================================================
// Per-source retrievers
// =============================================================================

/// FTS on PostgreSQL observations.
///
/// Enhanced for consolidation: also retrieves mental models, prefers them over
/// raw observations for the same topic, and records access on retrieved results.
async fn retrieve_observations(pg: &PgDb, query: &str, limit: i64) -> Vec<MemoryResult> {
    // Search regular observations
    let search_results = match pg.search_observations(query, None, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("Unified memory: observations retrieval failed: {}", e);
            return Vec::new();
        }
    };

    // Also fetch mental models (they're observations with is_mental_model=true).
    // We use these to: (1) filter out raw observations that were consolidated,
    // (2) inject relevant mental models with a score boost.
    let mental_models = pg.get_mental_models(limit).await.unwrap_or_default();
    let mental_model_ids: std::collections::HashSet<i64> =
        mental_models.iter().map(|m| m.id).collect();
    let mental_model_source_ids: std::collections::HashSet<i64> = mental_models
        .iter()
        .flat_map(|m| m.consolidated_from.as_deref().unwrap_or(&[]))
        .copied()
        .collect();

    let max_rank = search_results
        .iter()
        .filter_map(|r| r.rank)
        .fold(f32::NEG_INFINITY, f32::max)
        .max(1.0);

    let mut results: Vec<MemoryResult> = search_results
        .into_iter()
        .filter(|r| {
            // Skip raw observations that have been consolidated into a mental model
            // AND skip mental models from FTS (we'll add them separately with a boost)
            !mental_model_source_ids.contains(&r.id) && !mental_model_ids.contains(&r.id)
        })
        .map(|r| {
            let fts_score = r.rank.unwrap_or(0.0) / max_rank;
            let ts = chrono::DateTime::parse_from_rfc3339(&r.valid_from)
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
            // Apply temporal relevance: weight FTS score by recency/currency/supersession
            let temporal = ts
                .map(|vf| {
                    let vu = r
                        .valid_until
                        .as_deref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&Utc));
                    crate::database::pg::observations::temporal_score(
                        &vf,
                        vu.as_ref(),
                        r.superseded_by,
                    )
                })
                .unwrap_or(1.0);
            let score = (fts_score as f64) * temporal;
            MemoryResult {
                id: r.id.to_string(),
                source: MemorySource::Observation,
                title: r.title,
                snippet: truncate_str(&r.content_preview, 500),
                source_score: score,
                fused_score: 0.0,
                timestamp: ts,
                memory_type: r.observation_type,
                found_by: vec![RetrievalStrategy::FullTextSearch],
            }
        })
        .collect();

    // Add relevant mental models with a score boost.
    // Use word-level matching instead of exact substring to handle multi-word queries.
    let query_words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(|w| w.to_string())
        .collect();
    for model in &mental_models {
        let text_lower = format!("{} {}", model.title, model.content).to_lowercase();
        let matching_words = query_words
            .iter()
            .filter(|w| text_lower.contains(w.as_str()))
            .count();
        // Require at least half the query words to match (minimum 1)
        let threshold = (query_words.len() / 2).max(1);
        if matching_words >= threshold {
            results.push(MemoryResult {
                id: model.id.to_string(),
                source: MemorySource::Observation,
                title: format!("[Mental Model] {}", model.title),
                snippet: truncate_str(&model.content, 500),
                source_score: (model.importance * 1.2).min(1.0), // 20% boost for mental models
                fused_score: 0.0,
                timestamp: Some(model.updated_at),
                memory_type: model.observation_type.clone(),
                found_by: vec![RetrievalStrategy::FullTextSearch],
            });
        }
    }

    // Record access on all retrieved observations (best-effort, inline)
    for r in &results {
        if let Ok(id) = r.id.parse::<i64>() {
            let _ = pg.record_observation_access(id).await;
        }
    }

    results
}

/// FTS on PostgreSQL activity timeline.
async fn retrieve_timeline(pg: &PgDb, query: &str, limit: i64) -> Vec<MemoryResult> {
    match pg.search_timeline(query, limit).await {
        Ok(rows) => {
            let max_rank = rows
                .iter()
                .filter_map(|r| r.rank)
                .fold(f32::NEG_INFINITY, f32::max)
                .max(1.0);

            rows.into_iter()
                .map(|r| {
                    let score = r.rank.unwrap_or(0.0) / max_rank;
                    let ts = chrono::DateTime::parse_from_rfc3339(&r.created_at)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc));
                    MemoryResult {
                        id: r.id.to_string(),
                        source: MemorySource::ActivityTimeline,
                        title: r
                            .window_title
                            .unwrap_or_else(|| truncate_str(&r.text_preview, 80)),
                        snippet: truncate_str(&r.text_preview, 500),
                        source_score: score as f64,
                        fused_score: 0.0,
                        timestamp: ts,
                        memory_type: r.source_type,
                        found_by: vec![RetrievalStrategy::FullTextSearch],
                    }
                })
                .collect()
        }
        Err(e) => {
            warn!("Unified memory: timeline retrieval failed: {}", e);
            Vec::new()
        }
    }
}

/// Keyword search on PostgreSQL task_knowledge (content + category).
async fn retrieve_knowledge(pg: &PgDb, query: &str, limit: i64) -> Vec<MemoryResult> {
    match pg.search_task_knowledge(query, limit).await {
        Ok(rows) => {
            let total = rows.len().max(1) as f64;
            rows.into_iter()
                .enumerate()
                .map(|(i, (id, category, content, confidence, created_at))| {
                    // Position-based score with confidence boost
                    let base_score = 1.0 - (i as f64 / total);
                    let conf_mult = match confidence.as_str() {
                        "high" => 1.2,
                        "low" => 0.8,
                        _ => 1.0,
                    };
                    let score = (base_score * conf_mult).min(1.0);
                    let ts = chrono::DateTime::parse_from_rfc3339(&created_at)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc));
                    MemoryResult {
                        id,
                        source: MemorySource::TaskKnowledge,
                        title: format!("{} knowledge", category),
                        snippet: truncate_str(&content, 500),
                        source_score: score,
                        fused_score: 0.0,
                        timestamp: ts,
                        memory_type: category,
                        found_by: vec![RetrievalStrategy::CategoryMatch],
                    }
                })
                .collect()
        }
        Err(e) => {
            warn!("Unified memory: PG knowledge retrieval failed: {}", e);
            Vec::new()
        }
    }
}

/// LIKE search via PG unified_search (replaces SQLite retrieve_sqlite).
async fn retrieve_pg_unified(
    pg: &crate::database::pg::PgDb,
    query: &str,
    limit: usize,
) -> Vec<MemoryResult> {
    match pg.unified_search(query, limit).await {
        Ok(results) => results
            .into_iter()
            .map(|r| {
                let source = match r.entity_type.as_str() {
                    "finding" => MemorySource::Finding,
                    "fix" => MemorySource::Fix,
                    "knowledge" => MemorySource::TaskKnowledge,
                    "error" => MemorySource::Error,
                    "rule" => MemorySource::Rule,
                    "workflow" => MemorySource::Workflow,
                    "ui_element" => MemorySource::UiElement,
                    _ => MemorySource::Finding,
                };
                MemoryResult {
                    id: r.entity_id,
                    source,
                    title: r.title,
                    snippet: truncate_str(&r.snippet, 500),
                    source_score: r.relevance_score,
                    fused_score: 0.0,
                    timestamp: None,
                    memory_type: r.entity_type,
                    found_by: vec![RetrievalStrategy::CategoryMatch],
                }
            })
            .collect(),
        Err(e) => {
            warn!("Unified memory: PG unified search failed: {}", e);
            Vec::new()
        }
    }
}

/// Embedding-based semantic similarity search via PostgreSQL.
///
/// Searches knowledge (PG hybrid search with embeddings), findings (text fallback),
/// and error events (text fallback) in PostgreSQL.
/// Requires the local embedding service to be available.
async fn retrieve_embeddings(pg: &PgDb, query: &str, limit: usize) -> Vec<MemoryResult> {
    use crate::database::embedding_client::EmbeddingClient;
    use crate::database::hybrid_search::HybridSearchConfig;

    // 1. Create EmbeddingClient and check availability
    let client = EmbeddingClient::new();
    if !client.is_available().await {
        return Vec::new();
    }

    // 2. Compute query embedding (async)
    let embedding = match client.compute_text_embedding(query).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Unified memory: embedding computation failed: {}", e);
            return Vec::new();
        }
    };

    let config = HybridSearchConfig {
        limit,
        min_similarity: 0.4,
        ..Default::default()
    };

    let mut all_results: Vec<MemoryResult> = Vec::new();

    // Search knowledge via PG hybrid search (has embedding support)
    match pg.hybrid_search_knowledge(&embedding, None, &config).await {
        Ok(knowledge) => {
            for sr in knowledge {
                all_results.push(MemoryResult {
                    id: sr.item.id,
                    source: MemorySource::TaskKnowledge,
                    title: format!("{} knowledge", sr.item.category),
                    snippet: truncate_str(&sr.item.content, 500),
                    source_score: sr.hybrid_score as f64,
                    fused_score: 0.0,
                    timestamp: chrono::DateTime::parse_from_rfc3339(&sr.item.created_at)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc)),
                    memory_type: sr.item.category,
                    found_by: vec![RetrievalStrategy::EmbeddingSimilarity],
                });
            }
        }
        Err(e) => warn!("Unified memory: PG knowledge hybrid search failed: {}", e),
    }

    // Search error events via PG query (returns JSON values)
    match pg
        .query_error_events(None, None, None, None, None, Some(limit as u32))
        .await
    {
        Ok(errors) => {
            let query_lower = query.to_lowercase();
            let total = errors.len().max(1) as f64;
            for (i, e) in errors.into_iter().enumerate() {
                // Filter by query text client-side
                let message = e.get("message").and_then(|v| v.as_str()).unwrap_or("");
                if !query_lower.is_empty() && !message.to_lowercase().contains(&query_lower) {
                    continue;
                }
                let score = 1.0 - (i as f64 / total);
                let severity = e
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let last_seen = e.get("last_seen_at").and_then(|v| v.as_str()).unwrap_or("");
                let id = e
                    .get("id")
                    .and_then(|v| v.as_i64())
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                all_results.push(MemoryResult {
                    id,
                    source: MemorySource::Error,
                    title: format!("[{}] {}", severity, truncate_str(message, 80)),
                    snippet: truncate_str(message, 500),
                    source_score: score * 0.7, // Discount text-only results vs embedding
                    fused_score: 0.0,
                    timestamp: chrono::DateTime::parse_from_rfc3339(last_seen)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc)),
                    memory_type: "error_event".to_string(),
                    found_by: vec![RetrievalStrategy::FullTextSearch],
                });
            }
        }
        Err(e) => warn!("Unified memory: PG error events search failed: {}", e),
    }

    // Sort by source_score descending and truncate
    all_results.sort_by(|a, b| {
        b.source_score
            .partial_cmp(&a.source_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);
    all_results
}

/// Graph node text search (uses existing KnowledgeGraph::search_nodes).
fn retrieve_graph(graph: &KnowledgeGraph, query: &str, limit: usize) -> Vec<MemoryResult> {
    let nodes = graph.search_nodes(query, limit);
    let total = nodes.len().max(1) as f64;

    nodes
        .into_iter()
        .enumerate()
        .map(|(i, node)| {
            // search_nodes has no scoring; assign descending score by position
            let score = 1.0 - (i as f64 / total);
            MemoryResult {
                id: node.entity_id.clone(),
                source: MemorySource::GraphNode,
                title: node.label.clone(),
                snippet: truncate_str(&format!("[{}] {}", node.kind.as_str(), node.label), 500),
                source_score: score,
                fused_score: 0.0,
                timestamp: node
                    .created_at
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
                memory_type: node.kind.as_str().to_string(),
                found_by: vec![RetrievalStrategy::GraphTraversal],
            }
        })
        .collect()
}

// =============================================================================
// Reciprocal Rank Fusion
// =============================================================================

/// Fuse results from multiple retrieval strategies using RRF.
///
/// RRF score for a document d across strategies S:
///   score(d) = sum_{s in S} 1 / (k + rank_s(d))
///
/// where k is a smoothing constant (typically 60) and rank_s(d) is the
/// 1-based rank of d in strategy s's result list.
pub fn reciprocal_rank_fusion(result_sets: Vec<Vec<MemoryResult>>, k: f64) -> Vec<MemoryResult> {
    // Map: (source, id) -> (merged result, cumulative rrf score)
    let mut merged: HashMap<(MemorySource, String), MemoryResult> = HashMap::new();
    let mut scores: HashMap<(MemorySource, String), f64> = HashMap::new();

    for result_set in &result_sets {
        for (rank_0, result) in result_set.iter().enumerate() {
            let key = (result.source, result.id.clone());
            let rrf_contrib = 1.0 / (k + (rank_0 + 1) as f64);

            *scores.entry(key.clone()).or_insert(0.0) += rrf_contrib;

            merged
                .entry(key)
                .and_modify(|existing| {
                    // Merge found_by strategies
                    for strategy in &result.found_by {
                        if !existing.found_by.contains(strategy) {
                            existing.found_by.push(*strategy);
                        }
                    }
                    // Keep the higher source_score
                    if result.source_score > existing.source_score {
                        existing.source_score = result.source_score;
                    }
                })
                .or_insert_with(|| result.clone());
        }
    }

    // Apply fused scores and collect
    let mut results: Vec<MemoryResult> = merged
        .into_iter()
        .map(|(key, mut result)| {
            result.fused_score = scores[&key];
            result
        })
        .collect();

    // Normalize fused scores to 0.0-1.0
    let max_score = results
        .iter()
        .map(|r| r.fused_score)
        .fold(f64::NEG_INFINITY, f64::max);
    if max_score > 0.0 {
        for r in &mut results {
            r.fused_score /= max_score;
        }
    }

    // Sort by fused_score DESC
    results.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    results
}

// =============================================================================
// Unified query entry point
// =============================================================================

/// Helper: check if a source is enabled in the query params.
fn source_enabled(sources: &Option<Vec<MemorySource>>, s: MemorySource) -> bool {
    sources.as_ref().map_or(true, |list| list.contains(&s))
}

/// Query all memory stores in parallel, fuse with RRF, and return ranked results.
pub async fn query_memory(
    params: &UnifiedMemoryQuery,
    pg: &PgDb,
    graph: Option<&KnowledgeGraph>,
) -> Result<Vec<MemoryResult>, String> {
    let limit = params.limit.max(1);
    let query = &params.query;
    let pg_limit = limit as i64;

    // Determine which sources to query
    let want_observations = source_enabled(&params.sources, MemorySource::Observation);
    let want_timeline = source_enabled(&params.sources, MemorySource::ActivityTimeline);
    let want_knowledge = source_enabled(&params.sources, MemorySource::TaskKnowledge);
    let want_unified = source_enabled(&params.sources, MemorySource::Finding)
        || source_enabled(&params.sources, MemorySource::Fix)
        || source_enabled(&params.sources, MemorySource::Error)
        || source_enabled(&params.sources, MemorySource::Rule)
        || source_enabled(&params.sources, MemorySource::Workflow)
        || source_enabled(&params.sources, MemorySource::UiElement);
    let want_embeddings = source_enabled(&params.sources, MemorySource::TaskKnowledge)
        || source_enabled(&params.sources, MemorySource::Error);
    let want_graph = source_enabled(&params.sources, MemorySource::GraphNode) && graph.is_some();

    // Fan out parallel retrievals using tokio::join!
    let (obs_results, timeline_results, knowledge_results, unified_results, embedding_results) = tokio::join!(
        async {
            if want_observations {
                retrieve_observations(pg, query, pg_limit).await
            } else {
                Vec::new()
            }
        },
        async {
            if want_timeline {
                retrieve_timeline(pg, query, pg_limit).await
            } else {
                Vec::new()
            }
        },
        async {
            if want_knowledge {
                retrieve_knowledge(pg, query, pg_limit).await
            } else {
                Vec::new()
            }
        },
        async {
            if want_unified {
                retrieve_pg_unified(pg, query, limit).await
            } else {
                Vec::new()
            }
        },
        async {
            if want_embeddings {
                retrieve_embeddings(pg, query, limit).await
            } else {
                Vec::new()
            }
        },
    );

    // Graph retrieval is sync, run it directly
    let graph_results = if want_graph {
        if let Some(g) = graph {
            retrieve_graph(g, query, limit)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Collect all non-empty result sets for RRF fusion
    let mut result_sets: Vec<Vec<MemoryResult>> = Vec::new();
    if !obs_results.is_empty() {
        result_sets.push(obs_results);
    }
    if !timeline_results.is_empty() {
        result_sets.push(timeline_results);
    }
    if !knowledge_results.is_empty() {
        result_sets.push(knowledge_results);
    }
    if !unified_results.is_empty() {
        result_sets.push(unified_results);
    }
    if !embedding_results.is_empty() {
        result_sets.push(embedding_results);
    }
    if !graph_results.is_empty() {
        result_sets.push(graph_results);
    }

    // Fuse with RRF (k=60 standard smoothing constant)
    let mut fused = reciprocal_rank_fusion(result_sets, 60.0);

    // Apply temporal filters
    if let Some(from) = params.from {
        fused.retain(|r| r.timestamp.map_or(true, |t| t >= from));
    }
    if let Some(to) = params.to {
        fused.retain(|r| r.timestamp.map_or(true, |t| t <= to));
    }

    // Apply min_score filter
    if let Some(min_score) = params.min_score {
        fused.retain(|r| r.fused_score >= min_score);
    }

    // Apply source filter (post-fusion, some results from unified search may
    // have mapped to sources the caller didn't request)
    if let Some(ref sources) = params.sources {
        fused.retain(|r| sources.contains(&r.source));
    }

    // Truncate to limit
    fused.truncate(limit);

    Ok(fused)
}

/// Query memory with tiered reasoning enrichment.
///
/// Dispatches by reasoning level:
/// - Minimal: Just `query_memory()` results
/// - Low: + graph neighborhood enrichment for top results
/// - Medium: + causal context from graph traversal
/// - High: + LLM synthesis
/// - Max: + full research report via LLM
pub async fn query_memory_reasoned(
    params: &UnifiedMemoryQuery,
    pg: &PgDb,
    graph: Option<&KnowledgeGraph>,
) -> Result<ReasonedMemoryResponse, String> {
    // Always start with base results
    let results = query_memory(params, pg, graph).await?;
    let level = params.reasoning_level;

    if level == ReasoningLevel::Minimal {
        return Ok(ReasonedMemoryResponse {
            results,
            reasoning_level: level,
            neighborhood_context: None,
            causal_context: None,
            synthesis: None,
        });
    }

    // Low+: Graph neighborhood enrichment for top results
    let neighborhood_context = if graph.is_some() {
        let g = graph.unwrap();
        let top_n = results.iter().take(5);
        let mut snippets = Vec::new();
        for result in top_n {
            // Build graph node key from source and id
            let node_key = build_node_key(&result.source, &result.id, &result.memory_type);
            if let Some(neighborhood) = g.neighborhood(&node_key, 1) {
                let neighbors: Vec<NeighborSummary> = neighborhood
                    .neighbors
                    .iter()
                    .take(8)
                    .map(|n| NeighborSummary {
                        label: n.node.label.clone(),
                        kind: n.node.kind.as_str().to_string(),
                        edge_label: n.edge.label.clone().unwrap_or_default(),
                        direction: format!("{:?}", n.direction),
                        distance: n.distance,
                    })
                    .collect();
                if !neighbors.is_empty() {
                    snippets.push(NeighborhoodSnippet {
                        result_id: result.id.clone(),
                        source: result.source,
                        neighbors,
                    });
                }
            }
        }
        if snippets.is_empty() {
            None
        } else {
            Some(snippets)
        }
    } else {
        None
    };

    if level == ReasoningLevel::Low {
        return Ok(ReasonedMemoryResponse {
            results,
            reasoning_level: level,
            neighborhood_context,
            causal_context: None,
            synthesis: None,
        });
    }

    // Medium+: Causal context from graph traversal
    let causal_context = if graph.is_some() {
        let g = graph.unwrap();
        let top_n = results.iter().take(3);
        let mut chains = Vec::new();
        for result in top_n {
            let node_key = build_node_key(&result.source, &result.id, &result.memory_type);
            let paths = g.trace_root_causes(&node_key, 3);
            if !paths.is_empty() {
                let root_causes: Vec<String> = paths
                    .iter()
                    .filter_map(|p| p.nodes.last().map(|n| n.label.clone()))
                    .collect();
                let path_summary = paths
                    .iter()
                    .take(3)
                    .map(|p| {
                        p.nodes
                            .iter()
                            .map(|n| n.label.as_str())
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                chains.push(CausalChain {
                    result_id: result.id.clone(),
                    source: result.source,
                    root_causes,
                    path_summary,
                });
            }
        }
        if chains.is_empty() {
            None
        } else {
            Some(chains)
        }
    } else {
        None
    };

    if level == ReasoningLevel::Medium {
        return Ok(ReasonedMemoryResponse {
            results,
            reasoning_level: level,
            neighborhood_context,
            causal_context,
            synthesis: None,
        });
    }

    // High/Max: LLM synthesis
    let synthesis = synthesize_results(&params.query, &results, &neighborhood_context, &causal_context, level).await;

    Ok(ReasonedMemoryResponse {
        results,
        reasoning_level: level,
        neighborhood_context,
        causal_context,
        synthesis,
    })
}

/// Build a graph node key from a memory result's source and id.
/// Graph node keys follow the pattern "kind:entity_id".
fn build_node_key(source: &MemorySource, id: &str, memory_type: &str) -> String {
    let kind = match source {
        MemorySource::Observation => "observation",
        MemorySource::Finding => "finding",
        MemorySource::Fix => "fix",
        MemorySource::Error => "error",
        MemorySource::Rule => "rule",
        MemorySource::GraphNode => return id.to_string(), // Already a node key
        MemorySource::Workflow => "workflow",
        MemorySource::UiElement => "ui_element",
        MemorySource::ActivityTimeline => "timeline",
        MemorySource::TaskKnowledge => memory_type, // e.g. "pattern", "knowledge"
    };
    format!("{}:{}", kind, id)
}

/// Call the LLM to synthesize memory results into a coherent answer.
/// Returns None if the LLM is unavailable or the call fails.
async fn synthesize_results(
    query: &str,
    results: &[MemoryResult],
    neighborhood: &Option<Vec<NeighborhoodSnippet>>,
    causal: &Option<Vec<CausalChain>>,
    level: ReasoningLevel,
) -> Option<String> {
    use crate::ai_provider;
    use crate::ai_router::TaskContext;

    if results.is_empty() {
        return None;
    }

    // Build context from results
    let mut context_parts: Vec<String> = Vec::new();
    for (i, r) in results.iter().take(10).enumerate() {
        context_parts.push(format!(
            "{}. [{}] {} (score: {:.2})\n   {}",
            i + 1,
            format!("{:?}", r.source).to_lowercase(),
            r.title,
            r.fused_score,
            r.snippet
        ));
    }

    // Add neighborhood context if available
    if let Some(ref neighborhoods) = neighborhood {
        context_parts.push("\n--- Graph Neighborhood Context ---".to_string());
        for ns in neighborhoods.iter().take(5) {
            let neighbor_strs: Vec<String> = ns
                .neighbors
                .iter()
                .map(|n| format!("  - {} ({}) via '{}' [{}]", n.label, n.kind, n.edge_label, n.direction))
                .collect();
            context_parts.push(format!(
                "Result '{}': neighbors:\n{}",
                ns.result_id,
                neighbor_strs.join("\n")
            ));
        }
    }

    // Add causal context if available
    if let Some(ref chains) = causal {
        context_parts.push("\n--- Causal Context ---".to_string());
        for chain in chains.iter().take(3) {
            context_parts.push(format!(
                "Result '{}': root causes: [{}]\n  Path: {}",
                chain.result_id,
                chain.root_causes.join(", "),
                chain.path_summary
            ));
        }
    }

    let result_context = context_parts.join("\n\n");

    let prompt = if level == ReasoningLevel::Max {
        format!(
            "You are a memory research assistant. Given the user's query and retrieved memory results \
             with graph context, write a comprehensive research report that:\n\
             1. Directly answers the query\n\
             2. Synthesizes information across all sources\n\
             3. Identifies patterns and relationships\n\
             4. Notes any causal chains or root causes\n\
             5. Highlights contradictions or gaps\n\
             6. Suggests follow-up investigations\n\n\
             User query: {}\n\n\
             Retrieved memories:\n{}\n\n\
             Write a thorough research report:",
            query, result_context
        )
    } else {
        format!(
            "You are a memory synthesis assistant. Given the user's query and retrieved memory results, \
             write a concise synthesis (2-4 paragraphs) that directly answers the query by combining \
             information from the results. Focus on the most relevant findings and any causal relationships.\n\n\
             User query: {}\n\n\
             Retrieved memories:\n{}\n\n\
             Synthesis:",
            query, result_context
        )
    };

    let task_context = TaskContext::from_prompt(&prompt);

    let max_tokens = if level == ReasoningLevel::Max {
        Some(2048u32)
    } else {
        Some(1024u32)
    };

    // Call the LLM via spawn_blocking (same pattern as consolidation.rs)
    let prompt_clone = prompt.clone();
    let response = tokio::task::spawn_blocking(move || {
        ai_provider::run_prompt_with_model_override(
            &prompt_clone,
            &task_context,
            None,  // doctor_handle
            None,  // model_override (use default)
            None,  // provider_override
            Some(0.4), // moderate temperature
            max_tokens,
            None,  // fallback_model
            None,  // fallback_provider
        )
    })
    .await;

    match response {
        Ok(ai_response) => {
            if ai_response.success && !ai_response.output.is_empty() {
                Some(ai_response.output)
            } else {
                warn!(
                    "Memory synthesis LLM call failed: {:?}",
                    ai_response.error
                );
                None
            }
        }
        Err(e) => {
            warn!("Memory synthesis spawn_blocking failed: {}", e);
            None
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(id: &str, source: MemorySource, score: f64) -> MemoryResult {
        MemoryResult {
            id: id.to_string(),
            source,
            title: format!("Title {}", id),
            snippet: format!("Snippet {}", id),
            source_score: score,
            fused_score: 0.0,
            timestamp: None,
            memory_type: "test".to_string(),
            found_by: vec![RetrievalStrategy::FullTextSearch],
        }
    }

    #[test]
    fn test_rrf_single_strategy() {
        let set = vec![
            make_result("a", MemorySource::Observation, 1.0),
            make_result("b", MemorySource::Observation, 0.8),
            make_result("c", MemorySource::Observation, 0.5),
        ];
        let fused = reciprocal_rank_fusion(vec![set], 60.0);

        assert_eq!(fused.len(), 3);
        // First result should have highest score (rank 1 => 1/(60+1))
        assert_eq!(fused[0].id, "a");
        assert_eq!(fused[1].id, "b");
        assert_eq!(fused[2].id, "c");
        // Normalized: top should be 1.0
        assert!((fused[0].fused_score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_rrf_multi_strategy_boost() {
        // "a" appears in both sets (rank 1 and rank 2) => boosted
        // "b" appears only in set 1 (rank 2)
        // "d" appears only in set 2 (rank 1)
        let set1 = vec![
            make_result("a", MemorySource::Observation, 1.0),
            make_result("b", MemorySource::Observation, 0.5),
        ];
        let set2 = vec![
            make_result("d", MemorySource::Finding, 1.0),
            make_result("a", MemorySource::Observation, 0.9),
        ];
        let fused = reciprocal_rank_fusion(vec![set1, set2], 60.0);

        // "a" gets 1/(60+1) + 1/(60+2) = boosted above both "b" and "d"
        assert_eq!(fused[0].id, "a");
        assert!(fused[0].fused_score > fused[1].fused_score);
    }

    #[test]
    fn test_rrf_found_by_merge() {
        let set1 = vec![MemoryResult {
            found_by: vec![RetrievalStrategy::FullTextSearch],
            ..make_result("a", MemorySource::Finding, 1.0)
        }];
        let set2 = vec![MemoryResult {
            found_by: vec![RetrievalStrategy::GraphTraversal],
            ..make_result("a", MemorySource::Finding, 0.8)
        }];
        let fused = reciprocal_rank_fusion(vec![set1, set2], 60.0);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].found_by.len(), 2);
        assert!(fused[0]
            .found_by
            .contains(&RetrievalStrategy::FullTextSearch));
        assert!(fused[0]
            .found_by
            .contains(&RetrievalStrategy::GraphTraversal));
    }

    #[test]
    fn test_rrf_different_sources_not_merged() {
        // Same ID but different sources should stay separate
        let set1 = vec![make_result("123", MemorySource::Observation, 1.0)];
        let set2 = vec![make_result("123", MemorySource::Finding, 0.8)];
        let fused = reciprocal_rank_fusion(vec![set1, set2], 60.0);

        assert_eq!(fused.len(), 2);
    }

    #[test]
    fn test_rrf_empty_sets() {
        let fused = reciprocal_rank_fusion(vec![vec![], vec![], vec![]], 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_rrf_normalization() {
        let set = vec![
            make_result("a", MemorySource::Observation, 1.0),
            make_result("b", MemorySource::Observation, 0.5),
        ];
        let fused = reciprocal_rank_fusion(vec![set], 60.0);

        // All scores should be in [0, 1]
        for r in &fused {
            assert!(r.fused_score >= 0.0 && r.fused_score <= 1.0);
        }
        // Top result normalized to 1.0
        assert!((fused[0].fused_score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_rrf_keeps_highest_source_score() {
        let set1 = vec![make_result("a", MemorySource::Finding, 0.3)];
        let set2 = vec![make_result("a", MemorySource::Finding, 0.9)];
        let fused = reciprocal_rank_fusion(vec![set1, set2], 60.0);

        assert_eq!(fused.len(), 1);
        assert!((fused[0].source_score - 0.9).abs() < 1e-9);
    }

    #[test]
    fn test_source_parse_list() {
        let sources = MemorySource::parse_list("observation,finding,graph");
        assert_eq!(sources.len(), 3);
        assert!(sources.contains(&MemorySource::Observation));
        assert!(sources.contains(&MemorySource::Finding));
        assert!(sources.contains(&MemorySource::GraphNode));
    }

    #[test]
    fn test_source_parse_list_aliases() {
        let sources = MemorySource::parse_list("timeline,knowledge");
        assert!(sources.contains(&MemorySource::ActivityTimeline));
        assert!(sources.contains(&MemorySource::TaskKnowledge));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        let long = "a".repeat(600);
        let result = truncate_str(&long, 500);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 503); // 500 + "..."
    }

    #[test]
    fn test_temporal_filter_from() {
        use chrono::TimeZone;

        let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap();

        let set = vec![
            MemoryResult {
                timestamp: Some(t1),
                ..make_result("old", MemorySource::Observation, 1.0)
            },
            MemoryResult {
                timestamp: Some(t2),
                ..make_result("mid", MemorySource::Observation, 0.8)
            },
            MemoryResult {
                timestamp: Some(t3),
                ..make_result("new", MemorySource::Observation, 0.5)
            },
            // No timestamp — should be retained (map_or(true, ...))
            make_result("none", MemorySource::Finding, 0.3),
        ];

        let mut fused = reciprocal_rank_fusion(vec![set], 60.0);

        // Apply the same temporal filter that query_memory uses
        let from = Utc.with_ymd_and_hms(2025, 3, 1, 0, 0, 0).unwrap();
        fused.retain(|r| r.timestamp.map_or(true, |t| t >= from));

        let ids: Vec<&str> = fused.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"old"),
            "result before `from` should be excluded"
        );
        assert!(
            ids.contains(&"mid"),
            "result after `from` should be included"
        );
        assert!(
            ids.contains(&"new"),
            "result after `from` should be included"
        );
        assert!(
            ids.contains(&"none"),
            "result with no timestamp should be retained"
        );
    }

    #[test]
    fn test_temporal_filter_to() {
        use chrono::TimeZone;

        let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap();

        let set = vec![
            MemoryResult {
                timestamp: Some(t1),
                ..make_result("old", MemorySource::Observation, 1.0)
            },
            MemoryResult {
                timestamp: Some(t2),
                ..make_result("mid", MemorySource::Observation, 0.8)
            },
            MemoryResult {
                timestamp: Some(t3),
                ..make_result("new", MemorySource::Observation, 0.5)
            },
            make_result("none", MemorySource::Finding, 0.3),
        ];

        let mut fused = reciprocal_rank_fusion(vec![set], 60.0);

        // Apply the same temporal filter that query_memory uses
        let to = Utc.with_ymd_and_hms(2025, 9, 1, 0, 0, 0).unwrap();
        fused.retain(|r| r.timestamp.map_or(true, |t| t <= to));

        let ids: Vec<&str> = fused.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"old"),
            "result before `to` should be included"
        );
        assert!(
            ids.contains(&"mid"),
            "result before `to` should be included"
        );
        assert!(
            !ids.contains(&"new"),
            "result after `to` should be excluded"
        );
        assert!(
            ids.contains(&"none"),
            "result with no timestamp should be retained"
        );
    }

    #[test]
    fn test_source_filter_restricts() {
        // Replicate the source_enabled helper from query_memory
        fn source_enabled(sources: &Option<Vec<MemorySource>>, s: MemorySource) -> bool {
            sources.as_ref().map_or(true, |list| list.contains(&s))
        }

        // No filter — all sources enabled
        let no_filter: Option<Vec<MemorySource>> = None;
        assert!(source_enabled(&no_filter, MemorySource::Observation));
        assert!(source_enabled(&no_filter, MemorySource::Finding));
        assert!(source_enabled(&no_filter, MemorySource::GraphNode));

        // Explicit filter — only listed sources enabled
        let filter = Some(vec![MemorySource::Observation, MemorySource::Fix]);
        assert!(source_enabled(&filter, MemorySource::Observation));
        assert!(source_enabled(&filter, MemorySource::Fix));
        assert!(
            !source_enabled(&filter, MemorySource::Finding),
            "unlisted source should be disabled"
        );
        assert!(
            !source_enabled(&filter, MemorySource::GraphNode),
            "unlisted source should be disabled"
        );
        assert!(
            !source_enabled(&filter, MemorySource::ActivityTimeline),
            "unlisted source should be disabled"
        );
        assert!(
            !source_enabled(&filter, MemorySource::Error),
            "unlisted source should be disabled"
        );
    }
}
