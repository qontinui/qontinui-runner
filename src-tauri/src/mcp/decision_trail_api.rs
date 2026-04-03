//! HTTP endpoints for the Decision Trail system (architectural decision history).
//!
//! Provides CRUD, full-text search for both decisions and concept summaries.

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
        // Decisions
        .route("/decisions", post(save_decision_handler))
        .route("/decisions", get(list_decisions_handler))
        .route("/decisions/search", get(search_decisions_handler))
        .route("/decisions/{id}", get(get_decision_handler))
        .route("/decisions/{id}", put(update_decision_handler))
        .route("/decisions/{id}", delete(delete_decision_handler))
        .route("/decisions/{id}/status", put(update_status_handler))
        .route("/decisions/{id}/supersede", post(supersede_handler))
        // Concept Summaries
        .route("/concept-summaries", post(save_concept_handler))
        .route("/concept-summaries", get(list_concepts_handler))
        .route("/concept-summaries/search", get(search_concepts_handler))
        .route("/concept-summaries/{id}", get(get_concept_handler))
        .route("/concept-summaries/{id}", put(update_concept_handler))
        .route("/concept-summaries/{id}", delete(delete_concept_handler))
}

// ============================================================================
// Query parameter types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_max_results")]
    max_results: i64,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_max_results")]
    max_results: i64,
    category: Option<String>,
    scale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StatusInput {
    status: String,
}

#[derive(Debug, Deserialize)]
struct SupersedeInput {
    superseded_by: String,
}

fn default_max_results() -> i64 {
    50
}

// ============================================================================
// Decision Handlers
// ============================================================================

async fn save_decision_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateDecisionInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let id = pg.save_decision(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to save decision: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

async fn list_decisions_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ApiResponse<Vec<DecisionPreview>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = if let Some(ref category) = query.category {
        pg.list_decisions_by_category(category, query.max_results)
            .await
    } else if let Some(ref scale) = query.scale {
        pg.list_decisions_by_scale(scale, query.max_results).await
    } else {
        pg.list_decisions(query.max_results).await
    }
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list decisions: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(results)))
}

async fn search_decisions_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<DecisionSearchResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .search_decisions(&query.q, query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Search failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

async fn get_decision_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<Decision>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let decision = pg.get_decision(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get decision: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(decision)))
}

async fn update_decision_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(mut input): Json<UpdateDecisionInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    input.id = id;

    let updated = pg.update_decision(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update decision: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "updated": updated }),
    )))
}

async fn delete_decision_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let deleted = pg.delete_decision(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete decision: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "deleted": deleted }),
    )))
}

async fn update_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<StatusInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let updated = pg
        .update_decision_status(&id, &input.status)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update status: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "updated": updated }),
    )))
}

async fn supersede_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<SupersedeInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let updated = pg
        .supersede_decision(&id, &input.superseded_by)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to supersede decision: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "superseded": updated }),
    )))
}

// ============================================================================
// Concept Summary Handlers
// ============================================================================

async fn save_concept_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateConceptSummaryInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let id = pg.save_concept_summary(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to save concept summary: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
}

async fn list_concepts_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ApiResponse<Vec<ConceptSummaryPreview>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .list_concept_summaries(query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list concepts: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

async fn search_concepts_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<ConceptSummaryPreview>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let results = pg
        .search_concept_summaries(&query.q, query.max_results)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Concept search failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(results)))
}

async fn get_concept_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Option<ConceptSummaryRecord>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let concept = pg.get_concept_summary(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get concept: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(concept)))
}

async fn update_concept_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(mut input): Json<UpdateConceptSummaryInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    input.id = id;

    let updated = pg.update_concept_summary(&input).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update concept: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "updated": updated }),
    )))
}

async fn delete_concept_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let deleted = pg.delete_concept_summary(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete concept: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "deleted": deleted }),
    )))
}
