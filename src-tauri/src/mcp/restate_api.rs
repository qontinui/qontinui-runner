//! Restate API endpoints for awakeable resolution, status queries, and workflow control.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use super::types::{api_error, ApiResponse, ApiState};

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/restate/awakeables/{awakeable_id}/resolve",
            post(resolve_awakeable),
        )
        .route("/restate/health", get(restate_health))
        .route(
            "/restate/workflows/{execution_id}/stop",
            post(stop_restate_workflow),
        )
        .route(
            "/restate/workflows/{execution_id}/state",
            get(get_restate_workflow_state),
        )
}

#[derive(Debug, Deserialize)]
pub struct ResolveAwakeableRequest {
    pub approved: Option<bool>,
    pub comment: Option<String>,
    pub reviewer: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// Resolve a Restate awakeable (e.g., approval gate response).
///
/// POST /restate/awakeables/:awakeable_id/resolve
async fn resolve_awakeable(
    State(state): State<Arc<ApiState>>,
    Path(awakeable_id): Path<String>,
    Json(body): Json<ResolveAwakeableRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let restate_settings = crate::settings::load_settings().restate.clone();
    if !restate_settings.enabled {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Restate durable execution is not enabled")),
        ));
    }

    // Build the resolution payload
    let payload = if let Some(p) = body.payload {
        p
    } else {
        serde_json::json!({
            "approved": body.approved.unwrap_or(true),
            "comment": body.comment,
            "reviewer": body.reviewer,
        })
    };

    info!(
        awakeable_id = %awakeable_id,
        "Resolving Restate awakeable"
    );

    match crate::restate::launch::resolve_awakeable(
        &awakeable_id,
        &payload,
        &restate_settings.ingress_url(),
    )
    .await
    {
        Ok(()) => {
            // Update awakeable status in PG
            if let Err(e) = state
                .app_state
                .pg_db
                .resolve_restate_awakeable(&awakeable_id)
                .await
            {
                error!("Failed to update awakeable status in PG: {}", e);
            }

            Ok(Json(ApiResponse::success(serde_json::json!({
                "awakeable_id": awakeable_id,
                "status": "resolved"
            }))))
        }
        Err(e) => {
            error!("Failed to resolve awakeable {}: {}", awakeable_id, e);
            Err((StatusCode::BAD_GATEWAY, Json(api_error(e))))
        }
    }
}

/// Check Restate server health.
///
/// GET /restate/health
async fn restate_health(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let restate_settings = crate::settings::load_settings().restate.clone();

    let healthy = if restate_settings.enabled {
        crate::restate::lifecycle::is_restate_healthy_for(&restate_settings).await
    } else {
        false
    };

    Json(ApiResponse::success(serde_json::json!({
        "enabled": restate_settings.enabled,
        "healthy": healthy,
        "ingress_port": restate_settings.ingress_port,
        "admin_port": restate_settings.admin_port,
        "service_endpoint_port": restate_settings.service_endpoint_port,
    })))
}

/// Stop a Restate workflow.
///
/// POST /restate/workflows/:execution_id/stop
async fn stop_restate_workflow(
    State(_state): State<Arc<ApiState>>,
    Path(execution_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let restate_settings = crate::settings::load_settings().restate.clone();
    if !restate_settings.enabled {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Restate durable execution is not enabled")),
        ));
    }

    match crate::restate::launch::request_workflow_stop(
        &execution_id,
        &restate_settings.ingress_url(),
    )
    .await
    {
        Ok(()) => Ok(Json(ApiResponse::success(serde_json::json!({
            "execution_id": execution_id,
            "status": "stop_requested"
        })))),
        Err(e) => {
            error!("Failed to stop Restate workflow {}: {}", execution_id, e);
            Err((StatusCode::BAD_GATEWAY, Json(api_error(e))))
        }
    }
}

/// Get the state of a Restate-managed workflow.
///
/// GET /restate/workflows/:execution_id/state
async fn get_restate_workflow_state(
    State(_state): State<Arc<ApiState>>,
    Path(execution_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let restate_settings = crate::settings::load_settings().restate.clone();
    if !restate_settings.enabled {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Restate durable execution is not enabled")),
        ));
    }

    // Query Restate for the workflow state
    let url = format!(
        "{}/QontinuiWorkflow/{}/get_state",
        restate_settings.ingress_url(),
        execution_id
    );

    let client = reqwest::Client::new();
    match client
        .post(&url)
        .json(&serde_json::json!(null))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
            Ok(Json(ApiResponse::success(body)))
        }
        Ok(response) => {
            let status = response.status();
            Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Restate returned status {}", status))),
            ))
        }
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("Failed to query Restate: {}", e))),
        )),
    }
}
