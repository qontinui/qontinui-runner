//! Generation Rules HTTP endpoints.
//!
//! CRUD API for the `generation_rules` table — allows runtime management
//! of workflow generation rules without Rust recompilation.
//! Backed by PostgreSQL via `pg_db.*` methods in `database/pg/generation.rs`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::workflow_generation::rules::{
    GenerationRule, InsertRuleInput, ListRulesQuery, UpdateRuleInput,
};

/// GET /generation-rules
///
/// List generation rules with optional filters.
pub async fn list_rules_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListRulesQuery>,
) -> Result<Json<ApiResponse<Vec<GenerationRule>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rules = state
        .app_state
        .pg_db
        .list_all_rules(&query)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list generation rules: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(rules)))
}

/// GET /generation-rules/{id}
///
/// Get a single generation rule by ID.
pub async fn get_rule_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<GenerationRule>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rule = state
        .app_state
        .pg_db
        .get_rule_by_id(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get generation rule: {}", e))),
            )
        })?;

    match rule {
        Some(r) => Ok(Json(ApiResponse::success(r))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Generation rule '{}' not found", id))),
        )),
    }
}

/// POST /generation-rules
///
/// Create a new generation rule.
pub async fn create_rule_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<InsertRuleInput>,
) -> Result<Json<ApiResponse<GenerationRule>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rule = state
        .app_state
        .pg_db
        .upsert_rule(&input)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create generation rule: {}",
                    e
                ))),
            )
        })?;

    // Bump graph cache generation so the knowledge graph rebuilds on next access
    crate::mcp::graph_api::invalidate_graph_cache(&state, "generation_rule_mutation").await;

    info!(
        "HTTP: Created generation rule {} (agent={}, section={})",
        rule.id, rule.agent, rule.section
    );

    Ok(Json(ApiResponse::success(rule)))
}

/// PUT /generation-rules/{id}
///
/// Update an existing generation rule.
pub async fn update_rule_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRuleInput>,
) -> Result<Json<ApiResponse<GenerationRule>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rule = state
        .app_state
        .pg_db
        .update_rule(&id, &input)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update generation rule: {}",
                    e
                ))),
            )
        })?;

    // Bump graph cache generation so the knowledge graph rebuilds on next access
    crate::mcp::graph_api::invalidate_graph_cache(&state, "generation_rule_mutation").await;

    info!("HTTP: Updated generation rule {}", rule.id);

    Ok(Json(ApiResponse::success(rule)))
}

/// DELETE /generation-rules/{id}
///
/// Delete a generation rule (or set status=disabled).
pub async fn delete_rule_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let deleted = state.app_state.pg_db.delete_rule(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to delete generation rule: {}",
                e
            ))),
        )
    })?;

    if deleted {
        crate::mcp::graph_api::invalidate_graph_cache(&state, "delete_generation_rule").await;
        info!("HTTP: Deleted generation rule {}", id);
        Ok(Json(ApiResponse::success(true)))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Generation rule '{}' not found", id))),
        ))
    }
}

/// Register generation rules API routes.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;

    axum::Router::new()
        .route(
            "/generation-rules",
            get(list_rules_handler).post(create_rule_handler),
        )
        .route(
            "/generation-rules/{id}",
            get(get_rule_handler)
                .put(update_rule_handler)
                .delete(delete_rule_handler),
        )
}
