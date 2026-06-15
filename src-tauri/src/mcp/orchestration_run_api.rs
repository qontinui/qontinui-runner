//! HTTP API for Approach-D Conductor runs (Phase 3 control surface).
//!
//! Mirrors `orchestration_loop_api.rs`: each handler resolves the shared
//! `MultiLoopManager` + `PgDb` off [`ApiState`] and delegates to
//! `loop_engine`'s orchestration-run functions. Routes:
//!
//! - `POST   /orchestration/runs`        — start a run (body = start args)
//! - `GET    /orchestration/runs`        — list all runs (newest first)
//! - `GET    /orchestration/runs/{id}`   — run status (run + subtasks + phase)
//! - `DELETE /orchestration/runs/{id}`   — stop a run

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::orchestration_loop::commands::StartOrchestrationRunArgs;
use crate::orchestration_loop::conductor::OrchestrationRunConfig;
use crate::orchestration_loop::ledger::Run;
use crate::orchestration_loop::loop_engine::{self, OrchestrationRunStatus};

/// POST /orchestration/runs — start a conductor run.
async fn start_run(
    State(state): State<Arc<ApiState>>,
    Json(args): Json<StartOrchestrationRunArgs>,
) -> Result<Json<ApiResponse<Run>>, (StatusCode, Json<ApiResponse<()>>)> {
    let run_id = args.run_id.unwrap_or_else(Uuid::new_v4);
    let mut cfg = OrchestrationRunConfig::default();
    if let Some(c) = args.concurrency_cap {
        cfg.concurrency_cap = c.max(1);
    }
    if let Some(t) = args.tick_interval_secs {
        cfg.tick_interval_secs = t.max(1);
    }

    let run = loop_engine::start_orchestration_run(
        state.app_state.orchestration_loops.clone(),
        state.app_handle.clone(),
        state.app_state.pg_db.clone(),
        run_id,
        &args.goal,
        args.recipe.as_deref(),
        &args.phases,
        cfg,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "Failed to start orchestration run: {}",
                e
            ))),
        )
    })?;

    Ok(Json(ApiResponse::success(run)))
}

/// GET /orchestration/runs — list all runs.
async fn list_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<Run>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let runs = loop_engine::list_orchestration_runs(&state.app_state.pg_db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list orchestration runs: {}",
                    e
                ))),
            )
        })?;
    Ok(Json(ApiResponse::success(runs)))
}

/// GET /orchestration/runs/{id} — run status.
async fn run_status(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<ApiResponse<OrchestrationRunStatus>>, (StatusCode, Json<ApiResponse<()>>)> {
    let status = loop_engine::orchestration_run_status(
        state.app_state.orchestration_loops.clone(),
        &state.app_state.pg_db,
        run_id,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("No orchestration run: {}", e))),
        )
    })?;
    Ok(Json(ApiResponse::success(status)))
}

/// DELETE /orchestration/runs/{id} — stop a run.
async fn stop_run(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    loop_engine::stop_orchestration_run(
        state.app_state.orchestration_loops.clone(),
        &state.app_state.pg_db,
        run_id,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "Failed to stop orchestration run: {}",
                e
            ))),
        )
    })?;
    Ok(Json(ApiResponse::success(format!(
        "run {} stopped",
        run_id
    ))))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/orchestration/runs", get(list_runs).post(start_run))
        .route("/orchestration/runs/{id}", get(run_status).delete(stop_run))
}
