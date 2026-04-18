//! Server-mode workflow dispatch HTTP endpoint.
//!
//! Exposes `POST /api/workflows/run` for headless server runners started with
//! `QONTINUI_SERVER_MODE=1`. The handler loads the workflow by ID and spawns
//! it via `unified_workflow_executor::auto_run::launch_workflow_by_id`.
//!
//! The route is always registered, but the handler returns 404 when the
//! runner is not in server mode so desktop builds do not expose remote
//! dispatch.

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::ApiState;
use crate::unified_workflow_executor::auto_run::{launch_workflow_by_id, AutoRunDeps};

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/api/workflows/run", post(run_workflow))
}

#[derive(Debug, Deserialize)]
pub struct RunWorkflowRequest {
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub parent_task_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunWorkflowResponse {
    pub execution_id: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn server_mode_enabled() -> bool {
    std::env::var("QONTINUI_SERVER_MODE")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}

async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<RunWorkflowRequest>,
) -> Result<(StatusCode, Json<RunWorkflowResponse>), (StatusCode, Json<ErrorBody>)> {
    if !server_mode_enabled() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "Not found".to_string(),
            }),
        ));
    }

    let workflow_id = match body.workflow_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: "workflow_id is required".to_string(),
                }),
            ));
        }
    };

    info!(
        "Server-mode dispatch: workflow_id={} parent={:?}",
        workflow_id, body.parent_task_run_id
    );

    let deps = AutoRunDeps {
        app_state: state.app_state.clone(),
        config_storage: state.config_storage.clone(),
        app_handle: state.app_handle.clone(),
        pid_tracker: state.current_ai_pids.clone(),
    };

    let parent = body.parent_task_run_id.as_deref();

    let workflow_id_for_task = workflow_id.clone();
    let parent_owned = parent.map(|s| s.to_string());
    let result = tokio::task::spawn_blocking(move || {
        launch_workflow_by_id(deps, &workflow_id_for_task, parent_owned.as_deref())
    })
    .await;

    match result {
        Ok(Ok(execution_id)) => Ok((
            StatusCode::ACCEPTED,
            Json(RunWorkflowResponse { execution_id }),
        )),
        Ok(Err(e)) => {
            error!("Workflow dispatch failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody { error: e }),
            ))
        }
        Err(join_err) => {
            error!("Workflow dispatch task panicked: {}", join_err);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    error: format!("dispatch task failed: {}", join_err),
                }),
            ))
        }
    }
}
