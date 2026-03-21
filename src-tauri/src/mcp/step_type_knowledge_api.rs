//! Step Type Knowledge HTTP endpoints.
//!
//! CRUD API for the `step_type_knowledge` table — allows runtime management
//! of per-step-type best practices and pitfalls.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::workflow_generation::step_type_knowledge::{
    self, InsertKnowledgeInput, ListKnowledgeQuery, StepTypeKnowledge, UpdateKnowledgeInput,
};

/// GET /step-type-knowledge
///
/// List step type knowledge entries with optional filters.
pub async fn list_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListKnowledgeQuery>,
) -> Result<Json<ApiResponse<Vec<StepTypeKnowledge>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let entries = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| step_type_knowledge::list_knowledge(conn, &query))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list step type knowledge: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(entries)))
}

/// GET /step-type-knowledge/{id}
///
/// Get a single step type knowledge entry by ID.
pub async fn get_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<StepTypeKnowledge>>, (StatusCode, Json<ApiResponse<()>>)> {
    let entry = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| step_type_knowledge::get_knowledge(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get step type knowledge: {}",
                    e
                ))),
            )
        })?;

    match entry {
        Some(e) => Ok(Json(ApiResponse::success(e))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Step type knowledge '{}' not found", id))),
        )),
    }
}

/// POST /step-type-knowledge
///
/// Create a new step type knowledge entry.
pub async fn create_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<InsertKnowledgeInput>,
) -> Result<Json<ApiResponse<StepTypeKnowledge>>, (StatusCode, Json<ApiResponse<()>>)> {
    let entry = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| step_type_knowledge::insert_knowledge(conn, &input))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create step type knowledge: {}",
                    e
                ))),
            )
        })?;

    info!(
        "HTTP: Created step type knowledge {} (step_type={}, layer={})",
        entry.id, entry.step_type, entry.layer
    );

    Ok(Json(ApiResponse::success(entry)))
}

/// PUT /step-type-knowledge/{id}
///
/// Update an existing step type knowledge entry.
pub async fn update_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateKnowledgeInput>,
) -> Result<Json<ApiResponse<StepTypeKnowledge>>, (StatusCode, Json<ApiResponse<()>>)> {
    let entry = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| step_type_knowledge::update_knowledge(conn, &id, &input))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update step type knowledge: {}",
                    e
                ))),
            )
        })?;

    info!("HTTP: Updated step type knowledge {}", entry.id);

    Ok(Json(ApiResponse::success(entry)))
}

/// DELETE /step-type-knowledge/{id}
///
/// Delete a step type knowledge entry.
pub async fn delete_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let deleted = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| step_type_knowledge::delete_knowledge(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete step type knowledge: {}",
                    e
                ))),
            )
        })?;

    if deleted {
        info!("HTTP: Deleted step type knowledge {}", id);
        Ok(Json(ApiResponse::success(true)))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Step type knowledge '{}' not found", id))),
        ))
    }
}

/// Register step type knowledge API routes.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;

    axum::Router::new()
        .route(
            "/step-type-knowledge",
            get(list_knowledge_handler).post(create_knowledge_handler),
        )
        .route(
            "/step-type-knowledge/{id}",
            get(get_knowledge_handler)
                .put(update_knowledge_handler)
                .delete(delete_knowledge_handler),
        )
}
