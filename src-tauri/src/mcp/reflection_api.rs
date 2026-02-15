//! Reflection HTTP endpoints for MCP API
//!
//! Provides HTTP handlers for managing reflection fixes, effectiveness
//! evaluation, and reflection workflow history.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::reflection::storage::{self, ReflectionRunSummary};
use crate::reflection::types::{
    CreateReflectionFixInput, EffectivenessReport, ReflectionFix, UpdateEffectivenessInput,
    UpdateFixStatusInput,
};

/// Query parameters for listing reflection fixes.
#[derive(Debug, Deserialize)]
pub struct ListFixesQuery {
    pub workflow_name: Option<String>,
    pub status: Option<String>,
    pub effectiveness: Option<String>,
}

/// Response for the manual trigger endpoint.
#[derive(Debug, Serialize)]
pub struct TriggerResponse {
    pub reflection_task_run_id: String,
    pub status: String,
}

/// POST /reflection-fixes
///
/// Create a new reflection fix (called by reflection AI during analysis).
pub async fn create_fix_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateReflectionFixInput>,
) -> Result<Json<ApiResponse<ReflectionFix>>, (StatusCode, Json<ApiResponse<()>>)> {
    let fix = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::insert_fix(conn, &input))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create reflection fix: {}", e))),
            )
        })?;

    info!(
        "HTTP: Created reflection fix {} (type: {})",
        fix.id, fix.fix_type
    );

    Ok(Json(ApiResponse::success(fix)))
}

/// GET /reflection-fixes
///
/// List reflection fixes with optional filters.
pub async fn list_fixes_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListFixesQuery>,
) -> Result<Json<ApiResponse<Vec<ReflectionFix>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let workflow_name = query.workflow_name.as_deref().unwrap_or("");

    if workflow_name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "workflow_name query parameter is required".to_string(),
            )),
        ));
    }

    let fixes = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            storage::get_fixes_by_workflow_name(
                conn,
                workflow_name,
                query.status.as_deref(),
                query.effectiveness.as_deref(),
            )
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list fixes: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(fixes)))
}

/// GET /reflection-fixes/:id
///
/// Get a single reflection fix by ID.
pub async fn get_fix_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ReflectionFix>>, (StatusCode, Json<ApiResponse<()>>)> {
    let fix = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_fix(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get fix: {}", e))),
            )
        })?;

    match fix {
        Some(f) => Ok(Json(ApiResponse::success(f))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Reflection fix {} not found", id))),
        )),
    }
}

/// PUT /reflection-fixes/:id
///
/// Update the status of a reflection fix (applied/reverted/superseded).
pub async fn update_fix_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateFixStatusInput>,
) -> Result<Json<ApiResponse<ReflectionFix>>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::update_fix_status(conn, &id, &req.status))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update fix status: {}", e))),
            )
        })?;

    info!("HTTP: Updated fix {} status to {}", id, req.status);

    // Return updated fix
    let fix = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_fix(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get updated fix: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Fix {} not found", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(fix)))
}

/// PUT /reflection-fixes/:id/effectiveness
///
/// Update the effectiveness evaluation of a reflection fix.
pub async fn update_fix_effectiveness_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateEffectivenessInput>,
) -> Result<Json<ApiResponse<ReflectionFix>>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            storage::update_fix_effectiveness(
                conn,
                &id,
                &req.effectiveness,
                req.effectiveness_evidence.as_deref(),
            )
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update effectiveness: {}",
                    e
                ))),
            )
        })?;

    info!(
        "HTTP: Updated fix {} effectiveness to {}",
        id, req.effectiveness
    );

    let fix = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_fix(conn, &id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get updated fix: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Fix {} not found", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(fix)))
}

/// GET /task-runs/:id/reflection-fixes
///
/// Get all reflection fixes for a source task run.
pub async fn get_fixes_for_task_run_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<ReflectionFix>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let fixes = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_fixes_for_source_run(conn, &task_run_id))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get fixes: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(fixes)))
}

/// GET /reflection/effectiveness-report
///
/// Get an aggregated effectiveness report for a workflow.
#[derive(Debug, Deserialize)]
pub struct EffectivenessReportQuery {
    pub workflow_name: String,
}

pub async fn effectiveness_report_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EffectivenessReportQuery>,
) -> Result<Json<ApiResponse<EffectivenessReport>>, (StatusCode, Json<ApiResponse<()>>)> {
    let report = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_effectiveness_report(conn, &query.workflow_name))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to generate effectiveness report: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(report)))
}

/// GET /reflection/history
///
/// Get all reflection runs for a workflow.
#[derive(Debug, Deserialize)]
pub struct ReflectionHistoryQuery {
    pub workflow_name: String,
}

pub async fn reflection_history_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ReflectionHistoryQuery>,
) -> Result<Json<ApiResponse<Vec<ReflectionRunSummary>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let history = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| storage::get_reflection_history(conn, &query.workflow_name))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get reflection history: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(history)))
}

/// POST /reflection/trigger/:task_run_id
///
/// Manually trigger a reflection workflow for a completed task run.
pub async fn trigger_reflection_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<TriggerResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Check for already-running reflection workflow
    if let Ok(Some(existing_id)) = state.app_state.checkpoint_db.has_running_reflection_workflow() {
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "A reflection workflow is already running (task_id: {}). \
                 Please wait for it to complete before starting a new one.",
                existing_id
            ))),
        ));
    }

    let deps = crate::reflection::trigger::ReflectionDeps {
        app_state: state.app_state.clone(),
        config_storage: state.config_storage.clone(),
        app_handle: state.app_handle.clone(),
        pid_tracker: state.current_ai_pids.clone(),
    };

    let result = crate::reflection::trigger::launch_reflection(deps, task_run_id);

    match result {
        Ok(id) if id == "skipped" => Ok(Json(ApiResponse::success(TriggerResponse {
            reflection_task_run_id: String::new(),
            status: "skipped".to_string(),
        }))),
        Ok(id) => Ok(Json(ApiResponse::success(TriggerResponse {
            reflection_task_run_id: id,
            status: "launched".to_string(),
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to trigger reflection: {}",
                e
            ))),
        )),
    }
}

/// POST /reflection/evaluate
///
/// Trigger batch effectiveness evaluation for a workflow's fixes.
#[derive(Debug, Deserialize)]
pub struct EvaluateQuery {
    pub workflow_name: String,
}

pub async fn evaluate_fixes_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EvaluateQuery>,
) -> Result<Json<ApiResponse<EvaluateResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let results = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            crate::reflection::effectiveness::evaluate_pending_fixes(conn, &query.workflow_name)
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to evaluate fixes: {}", e))),
            )
        })?;

    let evaluated_count = results.len() as u32;
    let effective_count = results
        .iter()
        .filter(|r| {
            r.effectiveness == crate::reflection::types::FixEffectiveness::Effective
        })
        .count() as u32;

    Ok(Json(ApiResponse::success(EvaluateResponse {
        evaluated_count,
        effective_count,
    })))
}

#[derive(Debug, Serialize)]
pub struct EvaluateResponse {
    pub evaluated_count: u32,
    pub effective_count: u32,
}

/// Response for reflection settings.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReflectionSettingsResponse {
    pub reflection_enabled: bool,
}

/// GET /reflection/settings
///
/// Get the current reflection_enabled setting.
pub async fn get_reflection_settings_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ReflectionSettingsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let dev_mode = state
        .app_state
        .checkpoint_db
        .get_setting("dev_mode")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get setting: {}", e))),
            )
        })?;

    let reflection_enabled = dev_mode
        .and_then(|v| v.get("reflection_enabled").cloned())
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    Ok(Json(ApiResponse::success(ReflectionSettingsResponse {
        reflection_enabled,
    })))
}

/// PUT /reflection/settings
///
/// Update the reflection_enabled setting in the dev_mode JSON.
pub async fn update_reflection_settings_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ReflectionSettingsResponse>,
) -> Result<Json<ApiResponse<ReflectionSettingsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Read current dev_mode setting or start with empty object
    let mut dev_mode = state
        .app_state
        .checkpoint_db
        .get_setting("dev_mode")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get setting: {}", e))),
            )
        })?
        .unwrap_or_else(|| serde_json::json!({}));

    // Update the reflection_enabled field
    if let Some(obj) = dev_mode.as_object_mut() {
        obj.insert(
            "reflection_enabled".to_string(),
            serde_json::Value::Bool(req.reflection_enabled),
        );
    }

    // Write back
    state
        .app_state
        .checkpoint_db
        .set_setting("dev_mode", &dev_mode)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update setting: {}", e))),
            )
        })?;

    info!(
        "HTTP: Updated reflection_enabled to {}",
        req.reflection_enabled
    );

    Ok(Json(ApiResponse::success(ReflectionSettingsResponse {
        reflection_enabled: req.reflection_enabled,
    })))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post, put};

    axum::Router::new()
        // CRUD for reflection fixes
        .route("/reflection-fixes", post(create_fix_handler))
        .route("/reflection-fixes", get(list_fixes_handler))
        .route("/reflection-fixes/:id", get(get_fix_handler))
        .route("/reflection-fixes/:id", put(update_fix_status_handler))
        .route(
            "/reflection-fixes/:id/effectiveness",
            put(update_fix_effectiveness_handler),
        )
        // Task run scoped
        .route(
            "/task-runs/:id/reflection-fixes",
            get(get_fixes_for_task_run_handler),
        )
        // Reflection management
        .route(
            "/reflection/effectiveness-report",
            get(effectiveness_report_handler),
        )
        .route("/reflection/history", get(reflection_history_handler))
        .route(
            "/reflection/trigger/:task_run_id",
            post(trigger_reflection_handler),
        )
        .route("/reflection/evaluate", post(evaluate_fixes_handler))
        // Reflection settings
        .route(
            "/reflection/settings",
            get(get_reflection_settings_handler),
        )
        .route(
            "/reflection/settings",
            put(update_reflection_settings_handler),
        )
}
