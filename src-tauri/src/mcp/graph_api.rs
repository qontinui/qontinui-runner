//! MCP HTTP endpoints for the knowledge graph and unified memory search.
//!
//! Provides HTTP handlers for graph summary, unified search, cross-run patterns,
//! workflow versions, step provenance, rule influence, pipeline events,
//! phase stats, pattern detection, fuzzy error matching, graph traversals,
//! and unified memory retrieval with Reciprocal Rank Fusion.

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
use crate::memory::unified_query::{self, MemorySource, UnifiedMemoryQuery};
use crate::reflection::{
    cross_run_learning, fuzzy_matching, graph_engine::KnowledgeGraph,
    graph_types::GraphSummary, unified_search,
};

use crate::database::types::{Observation, ObservationSearchResult};

// ============================================================================
// Observation loading helper (async PG fetch for sync graph build)
// ============================================================================

/// Fetch observations from PostgreSQL for graph enrichment.
/// Uses a single batch query instead of N+1 individual fetches.
/// Returns empty vectors if PG is unavailable — graph still builds from SQLite.
async fn fetch_observations_for_graph(
    pg_db: &crate::database::pg::PgDb,
) -> (Vec<ObservationSearchResult>, Vec<Observation>) {
    let pg = pg_db;

    // Single batch query for full observations (replaces N+1 pattern)
    let full = pg
        .get_all_observations_full(500)
        .await
        .unwrap_or_default();

    // Build preview versions for load_observations_from_pg (needs content_preview)
    let previews: Vec<ObservationSearchResult> = full
        .iter()
        .map(|obs| ObservationSearchResult {
            id: obs.id,
            title: obs.title.clone(),
            content_preview: obs.content.chars().take(300).collect(),
            observation_type: obs.observation_type.clone(),
            scope: obs.scope.clone(),
            topic_key: obs.topic_key.clone(),
            revision_count: obs.revision_count,
            project_id: obs.project_id.clone(),
            valid_from: obs.valid_from.clone(),
            valid_until: obs.valid_until.clone(),
            superseded_by: obs.superseded_by,
            created_at: obs.created_at.clone(),
            updated_at: obs.updated_at.clone(),
            rank: None,
        })
        .collect();

    (previews, full)
}

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
        .route("/graph/ui-failure-chain", get(ui_failure_chain_handler))
        .route("/graph/ui-fix-effectiveness", get(ui_fix_effectiveness_handler))
        .route("/graph/skill-metrics", get(skill_metrics_handler))
        // Unified memory search (RRF fusion across all stores)
        .route("/memory/search", get(memory_search_handler))
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

#[derive(Debug, Deserialize)]
struct MemorySearchQuery {
    q: String,
    limit: Option<usize>,
    from: Option<String>,
    to: Option<String>,
    sources: Option<String>,
    min_score: Option<f64>,
    /// If true, include per-source scores and found_by strategy list.
    explain: Option<bool>,
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

    // Pre-fetch observations from PG (async) before the sync graph build
    let (obs_previews, obs_full) = fetch_observations_for_graph(
        &state.app_state.pg_db,
    )
    .await;

    let summary = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            let mut graph =
                KnowledgeGraph::build_from_db(conn, workflow_name.as_deref())?;
            // Enrich with PG observations
            if !obs_previews.is_empty() {
                graph.load_observations_from_pg(&obs_previews);
                graph.link_observations(&obs_full);
            }
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
        .pg_db
        .get_active_patterns(workflow_name.as_deref())
        .await
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
        .pg_db
        .get_workflow_versions(&query.workflow_id)
        .await
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
    // NOTE: No PG equivalent for get_provenance_for_workflow; stays on SQLite
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
    // NOTE: No PG equivalent for get_rule_influences; stays on SQLite
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
    // NOTE: No PG equivalent for get_ineffective_rules; stays on SQLite
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
        .pg_db
        .get_pipeline_events(&query.task_run_id)
        .await
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
    // NOTE: No PG equivalent for get_phase_stats; stays on SQLite
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

// ============================================================================
// UI Bridge failure chain and fix effectiveness
// ============================================================================

#[derive(Debug, Deserialize)]
struct UiFailureChainQuery {
    element_id: String,
}

/// GET /graph/ui-failure-chain — trace causal chain behind a UI element's failures.
async fn ui_failure_chain_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<UiFailureChainQuery>,
) -> Result<Json<ApiResponse<Vec<crate::reflection::graph_types::GraphPath>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = state.app_state.checkpoint_db.clone();
    let element_id = query.element_id;

    match tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            let kg = crate::reflection::graph_engine::KnowledgeGraph::build_from_db(conn, None)?;
            Ok(kg.trace_ui_failure_chain(&element_id))
        })
    })
    .await
    {
        Ok(Ok(paths)) => Ok(Json(ApiResponse::success(paths))),
        Ok(Err(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("{e}"))))),
    }
}

#[derive(Debug, Deserialize)]
struct UiFixEffectivenessQuery {
    #[serde(default = "default_effectiveness_limit")]
    limit: i64,
}

fn default_effectiveness_limit() -> i64 { 20 }

/// GET /graph/ui-fix-effectiveness — ranked fix effectiveness scores.
async fn ui_fix_effectiveness_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<UiFixEffectivenessQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::cross_run_ops::FixEffectivenessScore>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let db = state.app_state.checkpoint_db.clone();
    let limit = query.limit;

    match tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| crate::database::cross_run_ops::get_fix_effectiveness_scores(conn, limit))
    })
    .await
    {
        Ok(Ok(scores)) => Ok(Json(ApiResponse::success(scores))),
        Ok(Err(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("{e}"))))),
    }
}

// ============================================================================
// GET /graph/skill-metrics — Procedural skill system metrics
// ============================================================================

async fn skill_metrics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    match tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            // Counts by approval status
            let status_counts: Vec<(String, i64)> = conn
                .prepare(
                    r#"SELECT COALESCE(approval_status, 'pending') as status, COUNT(*)
                       FROM user_skills WHERE source = 'auto'
                       GROUP BY status ORDER BY COUNT(*) DESC"#,
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            let mut pending = 0i64;
            let mut approved = 0i64;
            let mut rejected = 0i64;
            for (status, count) in &status_counts {
                match status.as_str() {
                    "pending" => pending = *count,
                    "approved" => approved = *count,
                    "rejected" => rejected = *count,
                    _ => {}
                }
            }

            // Total usage across approved skills
            let total_usage: i64 = conn
                .query_row(
                    r#"SELECT COALESCE(SUM(usage_count), 0) FROM user_skills
                       WHERE source = 'auto' AND approval_status = 'approved'"#,
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            // Skill failure events count
            let failure_events: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM task_run_events WHERE event_type = 'skill_failure'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            // Auto-disabled count (skills that were approved then reverted to pending)
            let auto_disabled: i64 = conn
                .query_row(
                    r#"SELECT COUNT(DISTINCT event_subtype) FROM task_run_events
                       WHERE event_type = 'skill_failure'"#,
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            // Top skills by usage
            let top_skills: Vec<(String, String, i64)> = conn
                .prepare(
                    r#"SELECT slug, category, usage_count FROM user_skills
                       WHERE source = 'auto' AND approval_status = 'approved'
                       ORDER BY usage_count DESC LIMIT 5"#,
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            let top: Vec<serde_json::Value> = top_skills
                .into_iter()
                .map(|(slug, cat, usage)| {
                    serde_json::json!({"slug": slug, "category": cat, "usage_count": usage})
                })
                .collect();

            Ok(serde_json::json!({
                "total_auto_skills": pending + approved + rejected,
                "pending": pending,
                "approved": approved,
                "rejected": rejected,
                "total_usage": total_usage,
                "failure_events": failure_events,
                "unique_skills_failed": auto_disabled,
                "top_skills": top,
            }))
        })
    })
    .await
    {
        Ok(Ok(metrics)) => Ok(Json(ApiResponse::success(metrics))),
        Ok(Err(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("{e}"))))),
    }
}

// ============================================================================
// Endpoint: GET /memory/search — Unified memory retrieval with RRF fusion
// ============================================================================

/// Unified memory search response. In explain mode, includes per-source details.
#[derive(Debug, Serialize)]
struct MemorySearchResponse {
    results: Vec<unified_query::MemoryResult>,
    total: usize,
    query: String,
}

async fn memory_search_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemorySearchQuery>,
) -> Result<Json<ApiResponse<MemorySearchResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let sources = query.sources.as_deref().map(MemorySource::parse_list);

    let from = query
        .from
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let to = query
        .to
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let params = UnifiedMemoryQuery {
        query: query.q.clone(),
        limit: query.limit.unwrap_or(20),
        sources,
        from,
        to,
        min_score: query.min_score,
    };

    let pg = &state.app_state.pg_db;
    let db = &state.app_state.checkpoint_db;

    // Graph is expensive to build — only include if caller asks for graph_node source
    // or requests all sources (default).
    let want_graph = params
        .sources
        .as_ref()
        .map_or(true, |s| s.contains(&MemorySource::GraphNode));

    let graph = if want_graph {
        // Build graph in spawn_blocking (sync petgraph construction)
        let db_clone = db.clone();
        match tokio::task::spawn_blocking(move || {
            db_clone.with_conn(|conn| {
                KnowledgeGraph::build_from_db(conn, None).map_err(|e| format!("{e}"))
            })
        })
        .await
        {
            Ok(Ok(g)) => Some(g),
            _ => None,
        }
    } else {
        None
    };

    let results = unified_query::query_memory(&params, pg, db.clone(), graph.as_ref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Memory search failed: {}", e))),
            )
        })?;

    let total = results.len();
    let response = MemorySearchResponse {
        results,
        total,
        query: query.q,
    };

    Ok(Json(ApiResponse::success(response)))
}
