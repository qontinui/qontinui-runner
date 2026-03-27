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
        .route("/observations/{id}", get(get_handler))
        .route("/observations/{id}", put(update_handler))
        .route("/observations/{id}", delete(delete_handler))
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
