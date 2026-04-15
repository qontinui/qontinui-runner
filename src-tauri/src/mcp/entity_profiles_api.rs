//! HTTP endpoints for entity profiles (Honcho-inspired evolving representations).
//!
//! Provides listing, search, single-profile retrieval (with access recording),
//! generation, and stale-profile refresh.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
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
        .route("/memory/entity-profiles", get(list_handler))
        .route("/memory/entity-profiles/search", get(search_handler))
        .route("/memory/entity-profiles/{kind}/{id}", get(get_handler))
        .route("/memory/entity-profiles/generate", post(generate_handler))
        .route("/memory/entity-profiles/refresh", post(refresh_handler))
}

// ============================================================================
// Query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListQuery {
    entity_kind: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct GenerateInput {
    entity_kind: String,
    entity_id: String,
    entity_label: String,
}

#[derive(Debug, Deserialize)]
struct RefreshInput {
    #[serde(default = "default_stale_days")]
    stale_days: i32,
    #[serde(default = "default_max_profiles")]
    max_profiles: i64,
}

fn default_max_results() -> i64 {
    20
}

fn default_stale_days() -> i32 {
    7
}

fn default_max_profiles() -> i64 {
    50
}

// ============================================================================
// GET /memory/entity-profiles — list profiles, optionally filtered by kind
// ============================================================================

async fn list_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ApiResponse<Vec<EntityProfileSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;

    let results = if let Some(ref kind) = query.entity_kind {
        pg.get_profiles_by_kind(kind, query.max_results)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("List profiles failed: {}", e))),
                )
            })?
    } else {
        pg.get_all_profiles(query.max_results).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("List profiles failed: {}", e))),
            )
        })?
    };

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /memory/entity-profiles/search?q=...&max_results=20
// ============================================================================

async fn search_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<EntityProfileSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;

    let results = pg
        .search_entity_profiles(&query.q, query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Search profiles failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// GET /memory/entity-profiles/:kind/:id — single profile, records access
// ============================================================================

async fn get_handler(
    State(state): State<Arc<ApiState>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<EntityProfile>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    // Record the access
    let _ = pg.record_profile_access(&kind, &id).await;

    let profile = pg.get_entity_profile(&kind, &id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Get profile failed: {}", e))),
        )
    })?;

    match profile {
        Some(p) => Ok(Json(ApiResponse::success(p))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Entity profile {}:{} not found",
                kind, id
            ))),
        )),
    }
}

// ============================================================================
// POST /memory/entity-profiles/generate — generate a profile for an entity
// ============================================================================

async fn generate_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<GenerateInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let id = crate::memory::entity_profiles::generate_entity_profile(
        pg,
        &input.entity_kind,
        &input.entity_id,
        &input.entity_label,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Generate profile failed: {}", e))),
        )
    })?;

    crate::mcp::graph_api::invalidate_graph_cache(&state, "entity_profile_generated").await;

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

// ============================================================================
// POST /memory/entity-profiles/refresh — refresh stale profiles
// ============================================================================

async fn refresh_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<RefreshInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let refreshed = crate::memory::entity_profiles::refresh_stale_profiles(
        pg,
        input.stale_days,
        input.max_profiles,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Refresh profiles failed: {}", e))),
        )
    })?;

    if refreshed > 0 {
        crate::mcp::graph_api::invalidate_graph_cache(&state, "entity_profiles_refreshed").await;
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "refreshed": refreshed,
        "stale_days": input.stale_days,
        "max_profiles": input.max_profiles
    }))))
}
