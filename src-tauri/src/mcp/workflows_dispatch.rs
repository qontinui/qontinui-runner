//! Server-mode workflow dispatch HTTP endpoint.
//!
//! Exposes `POST /api/workflows/run` for headless server runners started with
//! `QONTINUI_SERVER_MODE=1`. The handler loads the workflow by ID and spawns
//! it via `unified_workflow_executor::auto_run::launch_workflow_by_id`.
//!
//! The route is always registered, but the handler returns 404 when the
//! runner is not in server mode so desktop builds do not expose remote
//! dispatch.
//!
//! # Authentication (server mode only)
//!
//! When server mode is enabled the handler requires
//! `Authorization: Bearer <token>`. The presented token must match **either**:
//!
//! * `QONTINUI_RUNNER_TOKEN` (the shared secret between this runner and web —
//!   also used by supervisor-triggered local dispatch), **or**
//! * the `dispatch_secret` returned by the web backend on the registration
//!   response (stored in [`ServerModeState`]).
//!
//! Both comparisons are done in constant time via [`subtle::ConstantTimeEq`].

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tracing::{error, info, warn};

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

/// Extract the plain bearer token value from an `Authorization` header.
/// Accepts `Authorization: Bearer <token>` with the literal word `Bearer`
/// (case-insensitive) and one space. Returns `None` on any malformed value.
fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let trimmed = raw.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let scheme = parts.next()?;
    let value = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Constant-time equality of two byte slices. Different lengths always
/// compare as not-equal (and in constant time w.r.t. the shorter of the two).
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
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

    // ----- Authentication -------------------------------------------------
    let presented = match extract_bearer(&headers) {
        Some(t) => t,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: "missing or malformed Authorization: Bearer header".to_string(),
                }),
            ));
        }
    };

    let runner_token_env = std::env::var("QONTINUI_RUNNER_TOKEN").ok();
    let dispatch_secret = match state.app_state.server_mode.as_ref() {
        Some(sm) => sm.dispatch_secret().await,
        None => None,
    };

    // 503 if neither secret is known yet.
    if runner_token_env
        .as_deref()
        .map(str::is_empty)
        .unwrap_or(true)
        && dispatch_secret.is_none()
    {
        warn!(
            "Server-mode dispatch rejected: no QONTINUI_RUNNER_TOKEN env var and no \
             dispatch_secret yet (runner has not completed web registration)"
        );
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "server mode without runner token — dispatch disabled".to_string(),
            }),
        ));
    }

    let env_match = runner_token_env
        .as_deref()
        .map(|t| !t.is_empty() && ct_eq(t, &presented))
        .unwrap_or(false);
    let secret_match = dispatch_secret
        .as_deref()
        .map(|s| ct_eq(s, &presented))
        .unwrap_or(false);

    if !env_match && !secret_match {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "bearer token did not match".to_string(),
            }),
        ));
    }

    // ----- Dispatch -------------------------------------------------------
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
