//! HTTP endpoints for the observations system (Engram-inspired persistent memory).
//!
//! Provides CRUD, full-text search, project context retrieval, and stats.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::sync::Arc;

use crate::database::types::*;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/observations", post(save_handler))
        .route("/observations/search", get(search_handler))
        .route("/observations/context", get(context_handler))
        .route("/observations/stats", get(stats_handler))
        .route("/observations/cleanup", post(cleanup_handler))
        .route("/observations/export", get(export_handler))
        .route("/observations/by-task-run/{task_run_id}", get(by_task_run_handler))
        .route("/observations/temporal-search", get(temporal_search_handler))
        .route("/observations/snapshot", get(snapshot_handler))
        .route("/observations/trends", get(trends_handler))
        .route("/observations/most-revised", get(most_revised_handler))
        .route("/observations/{id}", get(get_handler))
        .route("/observations/{id}", put(update_handler))
        .route("/observations/{id}", delete(delete_handler))
        .route("/observations/{id}/history", get(history_handler))
        .route("/observations/{id}/supersede", post(supersede_handler))
}

// ============================================================================
// Query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    project_id: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

fn default_max_results() -> i64 {
    20
}

#[derive(Debug, Deserialize)]
struct ContextQuery {
    project_id: String,
    observation_type: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

// ============================================================================
// POST /observations — save a new observation
// ============================================================================

async fn save_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateObservationInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let id = pg.save_observation(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to save observation: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

// ============================================================================
// GET /observations/search?q=...&project_id=...&max_results=20
// ============================================================================

async fn search_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<ObservationSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .search_observations(&query.q, query.project_id.as_deref(), query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Search failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /observations/context?project_id=...&observation_type=...&max_results=20
// ============================================================================

async fn context_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<ApiResponse<Vec<ObservationSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .get_project_context(
            &query.project_id,
            query.observation_type.as_deref(),
            query.max_results,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Context query failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /observations/stats
// ============================================================================

async fn stats_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ObservationTypeStat>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let stats = pg.get_observation_stats().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Stats query failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(stats)))
}

// ============================================================================
// GET /observations/by-task-run/:task_run_id
// ============================================================================

async fn by_task_run_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<ObservationSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg.get_observations_by_task_run(&task_run_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Query failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /observations/:id — full content (progressive disclosure)
// ============================================================================

async fn get_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<Observation>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let obs = pg.get_observation(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Get observation failed: {}", e))),
        )
    })?;

    match obs {
        Some(o) => Ok(Json(ApiResponse::success(o))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Observation {} not found", id))),
        )),
    }
}

// ============================================================================
// PUT /observations/:id — update title/content/type
// ============================================================================

async fn update_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(mut input): Json<UpdateObservationInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    input.id = id;

    let pg = &state.app_state.pg_db;

    let updated = pg.update_observation(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Update failed: {}", e))),
        )
    })?;

    match updated {
        Some(id) => Ok(Json(ApiResponse::success(serde_json::json!({ "id": id })))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Observation {} not found", id))),
        )),
    }
}

// ============================================================================
// DELETE /observations/:id — soft delete
// ============================================================================

async fn delete_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let deleted = pg.delete_observation(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Delete failed: {}", e))),
        )
    })?;

    if deleted {
        Ok(Json(ApiResponse::success(serde_json::json!({ "deleted": true }))))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Observation {} not found", id))),
        ))
    }
}

// ============================================================================
// POST /observations/cleanup — run retention policy
// ============================================================================

#[derive(Debug, Deserialize)]
struct CleanupQuery {
    /// Days to retain (default 90)
    #[serde(default = "default_retention_days")]
    retention_days: i32,
    /// Max revision count for cleanup candidates (default 1)
    #[serde(default = "default_max_revision")]
    max_revision_count: i32,
}

fn default_retention_days() -> i32 {
    90
}
fn default_max_revision() -> i32 {
    1
}

async fn cleanup_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CleanupQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let archived = pg
        .cleanup_stale_observations(query.retention_days, query.max_revision_count)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Cleanup failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "archived": archived,
        "retention_days": query.retention_days,
        "max_revision_count": query.max_revision_count
    }))))
}

// ============================================================================
// GET /observations/export — export all observations as JSON
// ============================================================================

async fn export_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<crate::database::types::Observation>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;

    let observations = pg.get_all_observations_full(10000).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Export failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(observations)))
}

// ============================================================================
// Temporal query types
// ============================================================================

#[derive(Debug, Deserialize)]
struct TemporalSearchQuery {
    q: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    as_of: Option<DateTime<Utc>>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct TrendsQuery {
    #[serde(default = "default_weeks")]
    weeks: i32,
}

fn default_weeks() -> i32 {
    8
}

#[derive(Debug, Deserialize)]
struct MostRevisedQuery {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct SupersedeInput {
    new_observation_id: i64,
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    as_of: DateTime<Utc>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

// ============================================================================
// GET /observations/temporal-search?q=...&from=...&to=...&as_of=...
// ============================================================================

async fn temporal_search_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TemporalSearchQuery>,
) -> Result<Json<ApiResponse<Vec<ObservationSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .search_observations_temporal(
            query.q.as_deref(),
            query.from,
            query.to,
            query.as_of,
            query.max_results,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Temporal search failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /observations/{id}/history — full revision history
// ============================================================================

async fn history_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<Vec<ObservationHistoryEntry>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let history = pg.get_observation_history(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("History query failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(history)))
}

// ============================================================================
// GET /observations/trends?weeks=8 — weekly observation counts by type
// ============================================================================

async fn trends_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TrendsQuery>,
) -> Result<Json<ApiResponse<Vec<WeeklyTrend>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let trends = pg.observation_trend(query.weeks).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Trends query failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(trends)))
}

// ============================================================================
// GET /observations/most-revised?from=...&to=...&max_results=20
// ============================================================================

async fn most_revised_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MostRevisedQuery>,
) -> Result<Json<ApiResponse<Vec<TopicRevisionCount>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .most_revised_topics(query.from, query.to, query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Most-revised query failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// POST /observations/{id}/supersede — mark as superseded by another
// ============================================================================

async fn supersede_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(input): Json<SupersedeInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let superseded = pg
        .supersede_observation(id, input.new_observation_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Supersede failed: {}", e))),
            )
        })?;

    if superseded {
        Ok(Json(ApiResponse::success(serde_json::json!({
            "superseded": true,
            "observation_id": id,
            "superseded_by": input.new_observation_id
        }))))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Observation {} not found", id))),
        ))
    }
}

// ============================================================================
// GET /observations/snapshot?as_of=...&max_results=20 — point-in-time snapshot
// ============================================================================

async fn snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<ApiResponse<Vec<crate::database::types::Observation>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let observations = pg
        .snapshot_at(query.as_of, query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Snapshot query failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(observations)))
}
