//! MCP HTTP endpoints for the knowledge graph.
//!
//! Provides HTTP handlers for graph summary, unified search, cross-run patterns,
//! workflow versions, step provenance, rule influence, pipeline events,
//! phase stats, pattern detection, fuzzy error matching, and graph traversals.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::database::{cross_run_ops, graph_ops};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::reflection::{
    cross_run_learning, fuzzy_matching, graph_engine::KnowledgeGraph,
    graph_types::GraphSummary, unified_search,
};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/graph/summary", get(summary_handler))
        .route("/graph/search", get(search_handler))
        .route("/graph/cross-run-patterns", get(cross_run_patterns_handler))
        .route("/graph/workflow-versions", get(workflow_versions_handler))
        .route("/graph/step-provenance", get(step_provenance_handler))
        .route("/graph/rule-influence", get(rule_influence_handler))
        .route("/graph/ineffective-rules", get(ineffective_rules_handler))
        .route("/graph/pipeline-events", get(pipeline_events_handler))
        .route("/graph/phase-stats", get(phase_stats_handler))
        .route("/graph/detect-patterns", post(detect_patterns_handler))
        .route("/graph/similar-errors", get(similar_errors_handler))
        // Graph traversal endpoints (expensive -- build graph on demand)
        .route("/graph/neighborhood", get(neighborhood_stub_handler))
        .route("/graph/paths", get(paths_stub_handler))
        .route("/graph/root-causes", get(root_causes_stub_handler))
        .route("/graph/impact", get(impact_stub_handler))
        .route("/graph/effectiveness", get(effectiveness_stub_handler))
}

// ============================================================================
// Query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SummaryQuery {
    workflow_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CrossRunPatternsQuery {
    workflow_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowIdQuery {
    workflow_id: String,
}

#[derive(Debug, Deserialize)]
struct RuleIdQuery {
    rule_id: String,
}

#[derive(Debug, Deserialize)]
struct IneffectiveRulesQuery {
    threshold: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TaskRunIdQuery {
    task_run_id: String,
}

#[derive(Debug, Deserialize)]
struct DetectPatternsQuery {
    workflow_name: String,
    task_run_id: String,
}

#[derive(Debug, Deserialize)]
struct SimilarErrorsQuery {
    description: String,
    min_similarity: Option<f64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GraphTraversalQuery {
    node_key: Option<String>,
    depth: Option<u32>,
}

// ============================================================================
// Response types for stubs
// ============================================================================

#[derive(Debug, Serialize)]
struct GraphStubResponse {
    message: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct DetectPatternsResponse {
    patterns_detected: u32,
    rules_disabled: u32,
    fixes_auto_applied: u32,
}

// ============================================================================
// Endpoint 1: GET /graph/summary
// ============================================================================

async fn summary_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SummaryQuery>,
) -> Result<Json<ApiResponse<GraphSummary>>, (StatusCode, Json<ApiResponse<()>>)> {
    let workflow_name = query.workflow_name;
    let summary = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            let graph =
                KnowledgeGraph::build_from_db(conn, workflow_name.as_deref())?;
            Ok(graph.summary())
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to build graph summary: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(summary)))
}

// ============================================================================
// Endpoint 2: GET /graph/search
// ============================================================================

async fn search_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchQuery>,
) -> Result<
    Json<ApiResponse<Vec<unified_search::UnifiedSearchResult>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let limit = query.limit.unwrap_or(20);
    let results = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| unified_search::unified_search(conn, &query.q, limit))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Search failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// Endpoint 3: GET /graph/cross-run-patterns
// ============================================================================

async fn cross_run_patterns_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CrossRunPatternsQuery>,
) -> Result<
    Json<ApiResponse<Vec<cross_run_ops::CrossRunPattern>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let workflow_name = query.workflow_name;
    let patterns = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            cross_run_ops::get_active_patterns(conn, workflow_name.as_deref())
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get cross-run patterns: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(patterns)))
}

// ============================================================================
// Endpoint 4: GET /graph/workflow-versions
// ============================================================================

async fn workflow_versions_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WorkflowIdQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::WorkflowVersion>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let versions = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| graph_ops::get_workflow_versions(conn, &query.workflow_id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get workflow versions: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(versions)))
}

// ============================================================================
// Endpoint 5: GET /graph/step-provenance
// ============================================================================

async fn step_provenance_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WorkflowIdQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::StepProvenance>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let provenance = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            graph_ops::get_provenance_for_workflow(conn, &query.workflow_id)
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get step provenance: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(provenance)))
}

// ============================================================================
// Endpoint 6: GET /graph/rule-influence
// ============================================================================

async fn rule_influence_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RuleIdQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::RuleInfluence>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let influences = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| graph_ops::get_rule_influences(conn, &query.rule_id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get rule influences: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(influences)))
}

// ============================================================================
// Endpoint 7: GET /graph/ineffective-rules
// ============================================================================

async fn ineffective_rules_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IneffectiveRulesQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::IneffectiveRule>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let threshold = query.threshold.unwrap_or(3);
    let rules = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| graph_ops::get_ineffective_rules(conn, threshold))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get ineffective rules: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(rules)))
}

// ============================================================================
// Endpoint 8: GET /graph/pipeline-events
// ============================================================================

async fn pipeline_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TaskRunIdQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::PipelineEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let events = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| graph_ops::get_pipeline_events(conn, &query.task_run_id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get pipeline events: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(events)))
}

// ============================================================================
// Endpoint 9: GET /graph/phase-stats
// ============================================================================

async fn phase_stats_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WorkflowIdQuery>,
) -> Result<
    Json<ApiResponse<Vec<graph_ops::PhaseStats>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let stats = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| graph_ops::get_phase_stats(conn, &query.workflow_id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get phase stats: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(stats)))
}

// ============================================================================
// Endpoint 10: POST /graph/detect-patterns
// ============================================================================

async fn detect_patterns_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DetectPatternsQuery>,
) -> Result<
    Json<ApiResponse<DetectPatternsResponse>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let (patterns_detected, rules_disabled, fixes_auto_applied) = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            cross_run_learning::post_run_analysis(
                conn,
                &query.workflow_name,
                &query.task_run_id,
            )
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to detect patterns: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(DetectPatternsResponse {
        patterns_detected,
        rules_disabled,
        fixes_auto_applied,
    })))
}

// ============================================================================
// Endpoint 11: GET /graph/similar-errors
// ============================================================================

async fn similar_errors_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SimilarErrorsQuery>,
) -> Result<
    Json<ApiResponse<Vec<fuzzy_matching::SimilarError>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let min_similarity = query.min_similarity.unwrap_or(0.6);
    let limit = query.limit.unwrap_or(10);
    let results = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            fuzzy_matching::find_similar_errors(
                conn,
                &query.description,
                None, // No embedding for HTTP text-only queries
                min_similarity,
                limit,
            )
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to find similar errors: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// Endpoints 12-16: Graph traversal stubs (expensive -- require full graph build)
// ============================================================================

async fn neighborhood_stub_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<GraphTraversalQuery>,
) -> Result<Json<ApiResponse<GraphStubResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(GraphStubResponse {
        message: "Graph neighborhood traversal requires building the full in-memory graph. \
                  This endpoint will be optimized with caching in a future release."
            .to_string(),
        status: "stub".to_string(),
    })))
}

async fn paths_stub_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<GraphTraversalQuery>,
) -> Result<Json<ApiResponse<GraphStubResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(GraphStubResponse {
        message: "Graph path finding requires building the full in-memory graph. \
                  This endpoint will be optimized with caching in a future release."
            .to_string(),
        status: "stub".to_string(),
    })))
}

async fn root_causes_stub_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<GraphTraversalQuery>,
) -> Result<Json<ApiResponse<GraphStubResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(GraphStubResponse {
        message: "Root cause tracing requires building the full in-memory graph. \
                  This endpoint will be optimized with caching in a future release."
            .to_string(),
        status: "stub".to_string(),
    })))
}

async fn impact_stub_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<GraphTraversalQuery>,
) -> Result<Json<ApiResponse<GraphStubResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(GraphStubResponse {
        message: "Impact analysis requires building the full in-memory graph. \
                  This endpoint will be optimized with caching in a future release."
            .to_string(),
        status: "stub".to_string(),
    })))
}

async fn effectiveness_stub_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<GraphTraversalQuery>,
) -> Result<Json<ApiResponse<GraphStubResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(GraphStubResponse {
        message: "Effectiveness ranking requires building the full in-memory graph. \
                  This endpoint will be optimized with caching in a future release."
            .to_string(),
        status: "stub".to_string(),
    })))
}
