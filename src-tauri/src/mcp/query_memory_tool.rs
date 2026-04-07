//! MCP tool for unified memory retrieval with RRF fusion.

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use super::graph_api::get_or_build_graph;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::memory::unified_query::{
    self, CausalChain, MemoryResult, MemorySource, NeighborhoodSnippet, ReasoningLevel,
    UnifiedMemoryQuery,
};

// ============================================================================
// Request/Response types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct QueryMemoryRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub sources: Option<Vec<String>>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub min_score: Option<f64>,
    /// Reasoning depth: minimal, low, medium, high, max.
    pub reasoning_level: Option<String>,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize)]
pub struct QueryMemoryResponse {
    pub results: Vec<MemoryResult>,
    pub total: usize,
    pub query: String,
    pub sources_queried: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<ReasoningLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_context: Option<Vec<CausalChain>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighborhood_context: Option<Vec<NeighborhoodSnippet>>,
}

#[derive(Debug, Serialize)]
pub struct MemoryToolDescriptor {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/mcp/memory/query", post(query_handler))
        .route("/mcp/memory/tool-descriptor", get(tool_descriptor_handler))
}

// ============================================================================
// POST /mcp/memory/query — Unified memory search (for agent tool calls)
// ============================================================================

async fn query_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<QueryMemoryRequest>,
) -> Result<Json<ApiResponse<QueryMemoryResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP memory query: '{}' (limit={})",
        request.query, request.limit
    );

    let sources = request.sources.as_ref().map(|list| {
        list.iter()
            .flat_map(|s| MemorySource::parse_list(s))
            .collect::<Vec<_>>()
    });

    let from = request
        .from
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let to = request
        .to
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let sources_queried: Vec<String> = sources.as_ref().map_or_else(
        || vec!["all".to_string()],
        |s| {
            s.iter()
                .map(|src| format!("{:?}", src).to_lowercase())
                .collect()
        },
    );

    let reasoning_level = request
        .reasoning_level
        .as_deref()
        .map(ReasoningLevel::from_str_opt)
        .unwrap_or(ReasoningLevel::Minimal);

    let params = UnifiedMemoryQuery {
        query: request.query.clone(),
        limit: request.limit,
        sources,
        from,
        to,
        min_score: request.min_score,
        reasoning_level,
    };

    let pg = &state.app_state.pg_db;

    // Optionally build graph (only if graph source enabled or all sources,
    // or if reasoning level needs graph enrichment)
    let want_graph = params
        .sources
        .as_ref()
        .is_none_or(|s| s.contains(&MemorySource::GraphNode))
        || reasoning_level != ReasoningLevel::Minimal;

    let graph = if want_graph {
        get_or_build_graph(&state, None).await.ok()
    } else {
        None
    };

    if reasoning_level != ReasoningLevel::Minimal {
        match unified_query::query_memory_reasoned(&params, pg, graph.as_deref()).await {
            Ok(reasoned) => {
                let total = reasoned.results.len();
                Ok(Json(ApiResponse::success(QueryMemoryResponse {
                    results: reasoned.results,
                    total,
                    query: request.query,
                    sources_queried,
                    reasoning_level: Some(reasoned.reasoning_level),
                    synthesis: reasoned.synthesis,
                    causal_context: reasoned.causal_context,
                    neighborhood_context: reasoned.neighborhood_context,
                })))
            }
            Err(e) => {
                error!("MCP memory query (reasoned) failed: {}", e);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Memory query failed: {}", e))),
                ))
            }
        }
    } else {
        match unified_query::query_memory(&params, pg, graph.as_deref()).await {
            Ok(results) => {
                let total = results.len();
                Ok(Json(ApiResponse::success(QueryMemoryResponse {
                    results,
                    total,
                    query: request.query,
                    sources_queried,
                    reasoning_level: None,
                    synthesis: None,
                    causal_context: None,
                    neighborhood_context: None,
                })))
            }
            Err(e) => {
                error!("MCP memory query failed: {}", e);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Memory query failed: {}", e))),
                ))
            }
        }
    }
}

// ============================================================================
// GET /mcp/memory/tool-descriptor — Returns the tool schema for agent registration
// ============================================================================

async fn tool_descriptor_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<MemoryToolDescriptor>> {
    Json(ApiResponse::success(MemoryToolDescriptor {
        name: "query_memory".to_string(),
        description: "Search all memory stores (observations, activity timeline, knowledge base, \
            findings, fixes, errors, rules, graph nodes) using Reciprocal Rank Fusion to produce \
            a single ranked result set."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return (default 10)",
                    "default": 10
                },
                "sources": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict to specific sources: observation, timeline, knowledge, finding, fix, error, rule, graph, workflow, ui_element"
                },
                "from": {
                    "type": "string",
                    "format": "date-time",
                    "description": "Only include results after this RFC3339 timestamp"
                },
                "to": {
                    "type": "string",
                    "format": "date-time",
                    "description": "Only include results before this RFC3339 timestamp"
                },
                "min_score": {
                    "type": "number",
                    "description": "Minimum fused score threshold (0.0-1.0)"
                },
                "reasoning_level": {
                    "type": "string",
                    "enum": ["minimal", "low", "medium", "high", "max"],
                    "description": "Reasoning depth: minimal (raw results), low (+graph neighbors), medium (+causal context), high (+LLM synthesis), max (+full research report)",
                    "default": "minimal"
                }
            },
            "required": ["query"]
        }),
    }))
}
