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
use tauri::Manager;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::reflection::architecture::{
    self, ComponentDetails, ComponentGraph, ImpactAnalysis, RebuildResult,
};
use crate::reflection::causal::{self, CausalChain, CausalEvent, CausalSummary};
use crate::reflection::context;
use crate::reflection::prediction::{self, ConvergenceMetrics, PredictedFix, ScoredKnowledge};
use crate::reflection::storage::{self, ReflectionRunSummary};
use crate::reflection::trends::{self, ComponentTrend, EffectivenessOverTime, WorkflowTrends};
use crate::reflection::trigger::ConvergenceStatus;
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
        .pg_db
        .save_reflection_fix(&input)
        .await
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
        .pg_db
        .get_fixes_by_workflow_name_filtered(
            workflow_name,
            query.status.as_deref(),
            query.effectiveness.as_deref(),
        )
        .await
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
        .pg_db
        .get_reflection_fix(&id)
        .await
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
        .pg_db
        .update_fix_status(&id, &req.status)
        .await
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
        .pg_db
        .get_reflection_fix(&id)
        .await
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
        .pg_db
        .update_fix_effectiveness(
            &id,
            &req.effectiveness,
            req.effectiveness_evidence.as_deref(),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update effectiveness: {}", e))),
            )
        })?;

    info!(
        "HTTP: Updated fix {} effectiveness to {}",
        id, req.effectiveness
    );

    let fix = state
        .app_state
        .pg_db
        .get_reflection_fix(&id)
        .await
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
        .pg_db
        .get_fixes_for_source_run(&task_run_id)
        .await
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
        .pg_db
        .get_effectiveness_report(&query.workflow_name)
        .await
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
        .pg_db
        .get_reflection_history(&query.workflow_name)
        .await
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
    if let Ok(Some(existing_id)) = state
        .app_state
        .checkpoint_db
        .has_running_reflection_workflow()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "A reflection workflow is already running (task_id: {}). \
                 Please wait for it to complete before starting a new one.",
                existing_id
            ))),
        ));
    }

    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());
    let deps = crate::reflection::trigger::ReflectionDeps {
        app_state: state.app_state.clone(),
        config_storage: state.config_storage.clone(),
        app_handle: state.app_handle.clone(),
        pid_tracker: state.current_ai_pids.clone(),
        session_manager,
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
            Json(api_error(format!("Failed to trigger reflection: {}", e))),
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
    // SQLite: complex function — batch effectiveness evaluation with multi-query analysis
    let db = state.app_state.checkpoint_db.clone();
    let wf_name = query.workflow_name.clone();
    let results = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            crate::reflection::effectiveness::evaluate_pending_fixes(conn, &wf_name)
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to evaluate fixes: {}", e))),
            )
        })?;

    let evaluated_count = results.len() as u32;
    let effective_count = results
        .iter()
        .filter(|r| r.effectiveness == crate::reflection::types::FixEffectiveness::Effective)
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

// =============================================================================
// Already-tried summary endpoint
// =============================================================================

/// Query parameters for the already-tried summary.
#[derive(Debug, Deserialize)]
pub struct AlreadyTriedQuery {
    pub workflow_name: String,
}

/// Response for the already-tried summary.
#[derive(Debug, Serialize)]
pub struct AlreadyTriedResponse {
    pub summary: String,
}

/// GET /reflection/already-tried-summary?workflow_name=...
///
/// Returns a structured summary of all previously attempted fixes for a workflow,
/// grouped by type with effectiveness labels and "DO NOT retry" warnings.
pub async fn already_tried_summary_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AlreadyTriedQuery>,
) -> Result<Json<ApiResponse<AlreadyTriedResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let fixes = state
        .app_state
        .pg_db
        .get_fixes_by_workflow_name(&query.workflow_name, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get fixes for already-tried summary: {}",
                    e
                ))),
            )
        })?;

    let summary = context::build_already_tried_summary(&fixes);

    Ok(Json(ApiResponse::success(AlreadyTriedResponse { summary })))
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
    let dev_mode = state.app_state.pg_db.get_setting("dev_mode").await
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
    let mut dev_mode = state.app_state.pg_db.get_setting("dev_mode").await
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
    state.app_state.pg_db.set_setting("dev_mode", &dev_mode).await
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

// =============================================================================
// Prediction API Endpoints (v99 Cognitive System Model)
// =============================================================================

/// Query parameters for fix prediction.
#[derive(Debug, Deserialize)]
pub struct PredictFixQuery {
    pub signature_hash: String,
}

/// GET /reflection/predict-fix?signature_hash=...
///
/// Predict which fix is most likely to resolve an error with the given signature.
pub async fn predict_fix_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PredictFixQuery>,
) -> Result<Json<ApiResponse<Option<PredictedFix>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — prediction model scoring with multi-table joins
    let db = state.app_state.checkpoint_db.clone();
    let sig = query.signature_hash.clone();
    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| prediction::predict_fix_for_error(conn, &sig))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to predict fix: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// Query parameters for convergence metrics.
#[derive(Debug, Deserialize)]
pub struct ConvergenceQuery {
    pub workflow_name: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "workflow".to_string()
}

/// GET /reflection/convergence?workflow_name=...&scope=workflow
///
/// Returns convergence metrics (gradient score 0.0-1.0) for a workflow.
pub async fn convergence_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConvergenceQuery>,
) -> Result<Json<ApiResponse<ConvergenceMetrics>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — convergence gradient computation over fix history
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let scope = query.scope.clone();
    let metrics = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| prediction::compute_convergence_score(conn, &wf, &scope))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to compute convergence: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(metrics)))
}

/// Query parameters for change velocity.
#[derive(Debug, Deserialize)]
pub struct ChangeVelocityQuery {
    pub component: String,
    #[serde(default = "default_window")]
    pub window: u32,
}

fn default_window() -> u32 {
    5
}

/// GET /reflection/change-velocity?component=...&window=5
///
/// Returns the change velocity (fixes per run) for a specific component.
pub async fn change_velocity_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ChangeVelocityQuery>,
) -> Result<Json<ApiResponse<f64>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — change velocity computation over fix history
    let db = state.app_state.checkpoint_db.clone();
    let component = query.component.clone();
    let window = query.window;
    let velocity = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| prediction::compute_change_velocity(conn, &component, window))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to compute change velocity: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(velocity)))
}

/// Query parameters for scored knowledge.
#[derive(Debug, Deserialize)]
pub struct ScoredKnowledgeQuery {
    pub workflow_name: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    10
}

/// GET /reflection/scored-knowledge?workflow_name=...&limit=10
///
/// Returns relevance-scored knowledge entries for a workflow.
pub async fn scored_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ScoredKnowledgeQuery>,
) -> Result<Json<ApiResponse<Vec<ScoredKnowledge>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — relevance scoring with multi-table knowledge join
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let limit = query.limit;
    let scored = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| prediction::score_knowledge_relevance(conn, &wf, limit))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to score knowledge: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(scored)))
}

// ---------------------------------------------------------------------------
// Causal chain endpoints (v100)
// ---------------------------------------------------------------------------

/// Query parameters for causal chain tracing.
#[derive(Debug, Deserialize)]
pub struct CausalTraceQuery {
    pub event_type: String,
    pub event_id: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

fn default_direction() -> String {
    "forward".to_string()
}

fn default_max_depth() -> u32 {
    10
}

/// Query parameters for causal events listing.
#[derive(Debug, Deserialize)]
pub struct CausalWorkflowQuery {
    pub workflow_name: String,
    #[serde(default = "default_causal_limit")]
    pub limit: u32,
}

fn default_causal_limit() -> u32 {
    20
}

/// Query parameters for building causal links.
#[derive(Debug, Deserialize)]
pub struct BuildCausalLinksQuery {
    pub task_run_id: String,
    pub workflow_name: String,
}

/// GET /reflection/causal-chain?event_type=...&event_id=...&direction=forward&max_depth=10
///
/// Traces a causal chain forward or backward from a given event.
pub async fn causal_chain_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CausalTraceQuery>,
) -> Result<Json<ApiResponse<CausalChain>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — recursive causal chain traversal (BFS/DFS)
    let db = state.app_state.checkpoint_db.clone();
    let event_type = query.event_type.clone();
    let event_id = query.event_id.clone();
    let direction = query.direction.clone();
    let max_depth = query.max_depth;
    let chain = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            if direction == "backward" {
                causal::trace_causal_chain_backward(conn, &event_type, &event_id, max_depth)
            } else {
                causal::trace_causal_chain_forward(conn, &event_type, &event_id, max_depth)
            }
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to trace causal chain: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(chain)))
}

/// GET /reflection/causal-events?workflow_name=...&limit=20
///
/// Lists causal events for a workflow.
pub async fn causal_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CausalWorkflowQuery>,
) -> Result<Json<ApiResponse<Vec<CausalEvent>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — causal event listing with workflow join
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let limit = query.limit;
    let events = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| causal::get_causal_events_for_workflow(conn, &wf, limit))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get causal events: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(events)))
}

/// POST /reflection/build-causal-links?task_run_id=...&workflow_name=...
///
/// Builds automated causal links from existing data in a task run.
pub async fn build_causal_links_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BuildCausalLinksQuery>,
) -> Result<Json<ApiResponse<u32>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — automated causal link construction from task run data
    let db = state.app_state.checkpoint_db.clone();
    let trid = query.task_run_id.clone();
    let wf = query.workflow_name.clone();
    let count = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| causal::build_automated_causal_links(conn, &trid, &wf))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to build causal links: {}", e))),
            )
        })?;

    info!(
        "HTTP: Built {} automated causal links for task_run={}, workflow={}",
        count, query.task_run_id, query.workflow_name
    );

    Ok(Json(ApiResponse::success(count)))
}

/// GET /reflection/causal-summary?workflow_name=...
///
/// Returns aggregate causal statistics for a workflow.
pub async fn causal_summary_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CausalWorkflowQuery>,
) -> Result<Json<ApiResponse<CausalSummary>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — aggregate causal statistics with multi-table joins
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let summary = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| causal::get_causal_summary(conn, &wf))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get causal summary: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(summary)))
}

// ---------------------------------------------------------------------------
// Architecture model endpoints (v101)
// ---------------------------------------------------------------------------

/// Query parameters for architecture graph.
#[derive(Debug, Deserialize)]
pub struct ArchitectureQuery {
    pub workflow_name: String,
}

/// Query parameters for component details.
#[derive(Debug, Deserialize)]
pub struct ComponentQuery {
    pub workflow_name: String,
    pub path: String,
}

/// Query parameters for impact analysis.
#[derive(Debug, Deserialize)]
pub struct ImpactQuery {
    pub workflow_name: String,
    pub component: String,
}

/// Query parameters for workflow trends.
#[derive(Debug, Deserialize)]
pub struct TrendsQuery {
    pub workflow_name: String,
    pub time_range: Option<String>,
}

/// Query parameters for component trends.
#[derive(Debug, Deserialize)]
pub struct ComponentTrendQuery {
    pub workflow_name: String,
    pub path: String,
    pub time_range: Option<String>,
}

/// Query parameters for effectiveness trend.
#[derive(Debug, Deserialize)]
pub struct EffectivenessTrendQuery {
    pub workflow_name: String,
    pub bucket: Option<String>,
    pub time_range: Option<String>,
}

/// GET /reflection/trends/effectiveness?workflow_name=...&bucket=week&time_range=30d
///
/// Returns time-bucketed effectiveness rate from reflection_fixes.
pub async fn effectiveness_trend_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EffectivenessTrendQuery>,
) -> Result<Json<ApiResponse<EffectivenessOverTime>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — time-bucketed effectiveness rate aggregation
    let db = state.app_state.checkpoint_db.clone();
    let bucket_type = query.bucket.as_deref().unwrap_or("week").to_string();
    let time_range = query.time_range.clone();
    let wf = query.workflow_name.clone();
    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            trends::get_effectiveness_over_time(conn, &wf, &bucket_type, time_range.as_deref())
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get effectiveness trends: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// GET /reflection/architecture?workflow_name=...
///
/// Returns the full component graph for a workflow.
pub async fn architecture_graph_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ArchitectureQuery>,
) -> Result<Json<ApiResponse<ComponentGraph>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — architecture component graph construction
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let graph = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| architecture::get_component_graph(conn, &wf))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get architecture graph: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(graph)))
}

/// POST /reflection/architecture/rebuild?workflow_name=...
///
/// Rebuilds the architecture model from reflection data.
pub async fn architecture_rebuild_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ArchitectureQuery>,
) -> Result<Json<ApiResponse<RebuildResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — rebuild architecture model from reflection data
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| architecture::rebuild_architecture_model(conn, &wf))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to rebuild architecture model: {}",
                    e
                ))),
            )
        })?;

    info!(
        "HTTP: Rebuilt architecture model for '{}': {} components, {} relationships",
        result.workflow_name, result.components_count, result.relationships_count
    );

    Ok(Json(ApiResponse::success(result)))
}

/// GET /reflection/architecture/component?workflow_name=...&path=...
///
/// Returns details for a single component.
pub async fn architecture_component_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ComponentQuery>,
) -> Result<Json<ApiResponse<ComponentDetails>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — component detail aggregation with fix history
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let path = query.path.clone();
    let details = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| architecture::get_component_details(conn, &wf, &path))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get component details: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(details)))
}

/// GET /reflection/architecture/impact?workflow_name=...&component=...
///
/// Returns impact analysis for a component (BFS, max 3 hops).
pub async fn architecture_impact_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ImpactQuery>,
) -> Result<Json<ApiResponse<ImpactAnalysis>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — BFS impact analysis (max 3 hops)
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let component = query.component.clone();
    let impact = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| architecture::get_impact_analysis(conn, &wf, &component))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get impact analysis: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(impact)))
}

/// GET /reflection/architecture/impact-graph?workflow_name=...&component=...
pub async fn architecture_impact_graph_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ImpactQuery>,
) -> Result<Json<ApiResponse<ImpactAnalysis>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — graph-enhanced BFS impact analysis
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let component = query.component.clone();
    let impact = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| architecture::get_impact_analysis_with_graph(conn, &wf, &component))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get graph-enhanced impact analysis: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(impact)))
}

/// GET /reflection/trends?workflow_name=...&time_range=7d
///
/// Returns workflow-level convergence trends over time.
pub async fn workflow_trends_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TrendsQuery>,
) -> Result<Json<ApiResponse<WorkflowTrends>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — workflow-level convergence trend computation
    let db = state.app_state.checkpoint_db.clone();
    let time_range = query.time_range.clone();
    let wf = query.workflow_name.clone();
    let wf_trends = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| trends::get_workflow_trends(conn, &wf, time_range.as_deref()))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get workflow trends: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(wf_trends)))
}

/// GET /reflection/trends/component?workflow_name=...&path=...&time_range=7d
///
/// Returns per-component health trends over time.
pub async fn component_trend_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ComponentTrendQuery>,
) -> Result<Json<ApiResponse<ComponentTrend>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — per-component health trend computation
    let db = state.app_state.checkpoint_db.clone();
    let time_range = query.time_range.clone();
    let wf = query.workflow_name.clone();
    let path = query.path.clone();
    let trend = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            trends::get_component_trend(conn, &wf, &path, time_range.as_deref())
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get component trend: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(trend)))
}

// ---------------------------------------------------------------------------
// Convergence status endpoint (Gap 5)
// ---------------------------------------------------------------------------

/// Query parameters for convergence status.
#[derive(Debug, Deserialize)]
pub struct ConvergenceStatusQuery {
    pub workflow_name: String,
}

/// GET /reflection/convergence-status?workflow_name=...
///
/// Returns a high-level convergence/stall status with human-readable explanation
/// and actionable recommendation.
pub async fn convergence_status_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConvergenceStatusQuery>,
) -> Result<Json<ApiResponse<ConvergenceStatus>>, (StatusCode, Json<ApiResponse<()>>)> {
    // SQLite: complex function — convergence/stall analysis with recommendation generation
    let db = state.app_state.checkpoint_db.clone();
    let wf = query.workflow_name.clone();
    let status = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            crate::reflection::trigger::analyze_convergence_status(conn, &wf)
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task join error: {}", e))),
        )
    })?
    .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to analyze convergence status: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(status)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post, put};

    axum::Router::new()
        // CRUD for reflection fixes
        .route(
            "/reflection-fixes",
            post(create_fix_handler).get(list_fixes_handler),
        )
        .route(
            "/reflection-fixes/{id}",
            get(get_fix_handler).put(update_fix_status_handler),
        )
        .route(
            "/reflection-fixes/{id}/effectiveness",
            put(update_fix_effectiveness_handler),
        )
        // Task run scoped
        .route(
            "/task-runs/{id}/reflection-fixes",
            get(get_fixes_for_task_run_handler),
        )
        // Reflection management
        .route(
            "/reflection/effectiveness-report",
            get(effectiveness_report_handler),
        )
        .route("/reflection/history", get(reflection_history_handler))
        .route(
            "/reflection/trigger/{task_run_id}",
            post(trigger_reflection_handler),
        )
        .route("/reflection/evaluate", post(evaluate_fixes_handler))
        // Already-tried summary
        .route(
            "/reflection/already-tried-summary",
            get(already_tried_summary_handler),
        )
        // Reflection settings
        .route(
            "/reflection/settings",
            get(get_reflection_settings_handler).put(update_reflection_settings_handler),
        )
        // Prediction API (v99 cognitive system model)
        .route("/reflection/predict-fix", get(predict_fix_handler))
        .route("/reflection/convergence", get(convergence_handler))
        .route(
            "/reflection/convergence-status",
            get(convergence_status_handler),
        )
        .route("/reflection/change-velocity", get(change_velocity_handler))
        .route(
            "/reflection/scored-knowledge",
            get(scored_knowledge_handler),
        )
        // Causal chain API (v100)
        .route("/reflection/causal-chain", get(causal_chain_handler))
        .route("/reflection/causal-events", get(causal_events_handler))
        .route("/reflection/causal-summary", get(causal_summary_handler))
        .route(
            "/reflection/build-causal-links",
            post(build_causal_links_handler),
        )
        // Architecture model API (v101)
        .route("/reflection/architecture", get(architecture_graph_handler))
        .route(
            "/reflection/architecture/rebuild",
            post(architecture_rebuild_handler),
        )
        .route(
            "/reflection/architecture/component",
            get(architecture_component_handler),
        )
        .route(
            "/reflection/architecture/impact",
            get(architecture_impact_handler),
        )
        .route(
            "/reflection/architecture/impact-graph",
            get(architecture_impact_graph_handler),
        )
        // Temporal trends API (v103)
        .route("/reflection/trends", get(workflow_trends_handler))
        .route("/reflection/trends/component", get(component_trend_handler))
        .route(
            "/reflection/trends/effectiveness",
            get(effectiveness_trend_handler),
        )
}
