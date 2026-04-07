//! HTTP endpoints for the memory consolidation system.
//!
//! Provides manual trigger, consolidation log, mental model listing,
//! decay preview, and memory health stats.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::database::types::*;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::memory::consolidation::{ConsolidationConfig, ConsolidationStats};
use crate::settings;

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/memory/consolidate", post(consolidate_handler))
        .route("/memory/consolidation-log", get(consolidation_log_handler))
        .route("/memory/mental-models", get(mental_models_handler))
        .route("/memory/decay-preview", get(decay_preview_handler))
        .route("/memory/health", get(health_handler))
        .route(
            "/memory/contradictions/scan",
            post(contradiction_scan_handler),
        )
        .route(
            "/memory/contradictions/log",
            get(contradiction_log_handler),
        )
        .route("/memory/dreamer/run", post(dreamer_run_handler))
        .route("/memory/dreamer/traces", get(dreamer_traces_handler))
        .route("/memory/dreamer/log", get(dreamer_log_handler))
}

// ============================================================================
// Query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
struct LogQuery {
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct DecayPreviewQuery {
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct MentalModelsQuery {
    #[serde(default = "default_mental_model_limit")]
    max_results: i64,
}

fn default_max_results() -> i64 {
    20
}

fn default_mental_model_limit() -> i64 {
    100
}

// ============================================================================
// POST /memory/consolidate — trigger consolidation manually
// ============================================================================

async fn consolidate_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ConsolidationStats>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let settings = crate::settings::load_settings();
    let config: ConsolidationConfig = (&settings.memory_consolidation).into();

    // Check cooldown
    if !crate::memory::consolidation::can_run_consolidation(pg, config.cooldown_hours).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(api_error(
                "Consolidation is on cooldown. Try again later.".to_string(),
            )),
        ));
    }

    match crate::memory::consolidation::run_consolidation(pg, &config).await {
        Ok(stats) => Ok(Json(ApiResponse {
            success: true,
            data: Some(stats),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Consolidation failed: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/consolidation-log — recent consolidation runs
// ============================================================================

async fn consolidation_log_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<ApiResponse<Vec<ConsolidationLogEntry>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg.get_consolidation_log(params.max_results).await {
        Ok(entries) => Ok(Json(ApiResponse {
            success: true,
            data: Some(entries),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get consolidation log: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/mental-models — list all mental models
// ============================================================================

async fn mental_models_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<MentalModelsQuery>,
) -> Result<Json<ApiResponse<Vec<MentalModelSummary>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg.get_mental_models(params.max_results).await {
        Ok(models) => {
            let summaries: Vec<MentalModelSummary> = models
                .into_iter()
                .map(|m| MentalModelSummary {
                    id: m.id,
                    title: m.title,
                    content: m.content,
                    observation_type: m.observation_type,
                    importance: m.importance,
                    access_count: m.access_count,
                    source_count: m.consolidated_from.as_ref().map(|v| v.len()).unwrap_or(0),
                    created_at: m.created_at.to_rfc3339(),
                    updated_at: m.updated_at.to_rfc3339(),
                })
                .collect();
            Ok(Json(ApiResponse {
                success: true,
                data: Some(summaries),
                error: None,
                error_detail: None,
            }))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get mental models: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/decay-preview — preview which observations would be archived
// ============================================================================

async fn decay_preview_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<DecayPreviewQuery>,
) -> Result<Json<ApiResponse<Vec<DecayPreviewEntry>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg.get_decay_preview(params.max_results).await {
        Ok(entries) => Ok(Json(ApiResponse {
            success: true,
            data: Some(entries),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get decay preview: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/health — memory health statistics
// ============================================================================

async fn health_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<MemoryHealthResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let stats = pg.get_memory_health_stats().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get memory health: {}", e))),
        )
    })?;

    let last_consolidation = pg.get_last_consolidation_time().await.ok().flatten();

    Ok(Json(ApiResponse {
        success: true,
        data: Some(MemoryHealthResponse {
            total_observations: stats.total_observations,
            total_mental_models: stats.total_mental_models,
            decay_queue_size: stats.decay_queue_size,
            avg_importance: stats.avg_importance,
            avg_access_count: stats.avg_access_count,
            last_consolidation: last_consolidation.map(|t| t.to_rfc3339()),
        }),
        error: None,
        error_detail: None,
    }))
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MentalModelSummary {
    id: i64,
    title: String,
    content: String,
    observation_type: String,
    importance: f64,
    access_count: i32,
    source_count: usize,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryHealthResponse {
    total_observations: i64,
    total_mental_models: i64,
    decay_queue_size: i64,
    avg_importance: Option<f64>,
    avg_access_count: Option<f64>,
    last_consolidation: Option<String>,
}

// ============================================================================
// POST /memory/contradictions/scan — trigger contradiction scan
// ============================================================================

#[derive(Debug, Deserialize)]
struct ContradictionScanQuery {
    #[serde(default = "default_contradiction_limit")]
    max_candidates: i64,
}

fn default_contradiction_limit() -> i64 {
    50
}

async fn contradiction_scan_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ContradictionScanQuery>,
) -> Result<Json<ApiResponse<ContradictionStats>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match crate::memory::contradiction::run_contradiction_scan(pg, params.max_candidates).await {
        Ok(stats) => Ok(Json(ApiResponse {
            success: true,
            data: Some(stats),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Contradiction scan failed: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/contradictions/log — recent contradiction resolutions
// ============================================================================

#[derive(Debug, Deserialize)]
struct ContradictionLogQuery {
    #[serde(default = "default_max_results")]
    limit: i64,
}

async fn contradiction_log_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ContradictionLogQuery>,
) -> Result<Json<ApiResponse<Vec<ContradictionResolution>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg.get_recent_resolutions(params.limit).await {
        Ok(resolutions) => Ok(Json(ApiResponse {
            success: true,
            data: Some(resolutions),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to get contradiction log: {}",
                e
            ))),
        )),
    }
}

// ============================================================================
// POST /memory/dreamer/run — trigger dreamer manually
// ============================================================================

async fn dreamer_run_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DreamerStats>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let s = settings::load_settings();
    let config: ConsolidationConfig = (&s.memory_consolidation).into();

    match crate::memory::consolidation::run_dreamer(pg, &config).await {
        Ok(stats) => Ok(Json(ApiResponse {
            success: true,
            data: Some(stats),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Dreamer run failed: {}", e))),
        )),
    }
}

// ============================================================================
// GET /memory/dreamer/traces — recent reasoning traces
// ============================================================================

#[derive(Debug, Deserialize)]
struct DreamerTracesQuery {
    #[serde(default)]
    reasoning_type: Option<String>,
    #[serde(default = "default_max_results")]
    limit: i64,
}

async fn dreamer_traces_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<DreamerTracesQuery>,
) -> Result<Json<ApiResponse<Vec<ReasoningTrace>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg
        .get_recent_traces(params.reasoning_type.as_deref(), params.limit)
        .await
    {
        Ok(traces) => Ok(Json(ApiResponse {
            success: true,
            data: Some(traces),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to get reasoning traces: {}",
                e
            ))),
        )),
    }
}

// ============================================================================
// GET /memory/dreamer/log — dreamer run history
// ============================================================================

#[derive(Debug, Deserialize)]
struct DreamerLogQuery {
    #[serde(default = "default_max_results")]
    limit: i64,
}

async fn dreamer_log_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<DreamerLogQuery>,
) -> Result<Json<ApiResponse<Vec<DreamerLogEntry>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    match pg.get_dreamer_log(params.limit).await {
        Ok(entries) => Ok(Json(ApiResponse {
            success: true,
            data: Some(entries),
            error: None,
            error_detail: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get dreamer log: {}", e))),
        )),
    }
}
