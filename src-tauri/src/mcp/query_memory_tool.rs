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

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::memory::unified_query::{self, MemoryResult, MemorySource, UnifiedMemoryQuery};
use super::graph_api::get_or_build_graph;

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
    info!("MCP memory query: '{}' (limit={})", request.query, request.limit);

    let sources = request.sources.as_ref().map(|list| {
        list.iter()
            .flat_map(|s| MemorySource::parse_list(s))
            .collect::<Vec<_>>()
    });

    let from = request.from.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let to = request.to.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let sources_queried: Vec<String> = sources.as_ref().map_or_else(
        || vec!["all".to_string()],
        |s| s.iter().map(|src| format!("{:?}", src).to_lowercase()).collect(),
    );

    let params = UnifiedMemoryQuery {
        query: request.query.clone(),
        limit: request.limit,
        sources,
        from,
        to,
        min_score: request.min_score,
    };

    let pg = &state.app_state.pg_db;

    // Optionally build graph (only if graph source enabled or all sources)
    let want_graph = params.sources.as_ref()
        .is_none_or(|s| s.contains(&MemorySource::GraphNode));

    let graph = if want_graph {
        get_or_build_graph(&state, None).await.ok()
    } else {
        None
    };

    match unified_query::query_memory(&params, pg, graph.as_deref()).await {
        Ok(results) => {
            let total = results.len();
            Ok(Json(ApiResponse::success(QueryMemoryResponse {
                results,
                total,
                query: request.query,
                sources_queried,
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
            a single ranked result set.".to_string(),
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
                }
            },
            "required": ["query"]
        }),
    }))
}
