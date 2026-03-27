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
    let config = ConsolidationConfig::default();

    // Check cooldown
    if !crate::memory::consolidation::can_run_consolidation(pg, config.cooldown_hours).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(api_error("Consolidation is on cooldown. Try again later.".to_string())),
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
