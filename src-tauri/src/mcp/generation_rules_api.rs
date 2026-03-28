//! Generation Rules HTTP endpoints.
//!
//! CRUD API for the `generation_rules` table — allows runtime management
//! of workflow generation rules without Rust recompilation.
//!
//! TODO: Wire to PG (pg_db.list_all_rules, get_rule_by_id, upsert_rule, update_rule, delete_rule).
//! PG methods exist in database/pg/generation.rs but axum 0.7/0.8 Handler trait clash
//! prevents compilation when async PG calls are used in these handlers. Fix after
//! async-graphql-axum upgrades to axum 0.8 (eliminates dual-version conflict).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::workflow_generation::rules::{
    self, GenerationRule, InsertRuleInput, ListRulesQuery, UpdateRuleInput,
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
        .checkpoint_db
        .with_conn(|conn| rules::list_rules(conn, &query))
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
        .checkpoint_db
        .with_conn(|conn| rules::get_rule(conn, &id))
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
        .checkpoint_db
        .with_conn(|conn| rules::insert_rule(conn, &input))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create generation rule: {}",
                    e
                ))),
            )
        })?;

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
        .checkpoint_db
        .with_conn(|conn| rules::update_rule(conn, &id, &input))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update generation rule: {}",
                    e
                ))),
            )
        })?;

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
    let deleted = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| rules::delete_rule(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete generation rule: {}",
                    e
                ))),
            )
        })?;

    if deleted {
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
