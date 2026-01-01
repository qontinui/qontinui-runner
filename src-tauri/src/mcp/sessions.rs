//! AI session management handlers for MCP API
//!
//! Provides handlers for managing AI sessions, continuation, and auto-continue.

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use tracing::{error, info, warn};

use super::types::{
    api_error, AiOutputSessionContext, ApiResponse, ApiState, AutoContinueSettingResponse,
    ForceContinueRequest, ForceContinueResponse, ResumableWorkflowInfo, ResumeWorkflowResponse,
    SetAutoContinueRequest, StartSessionRequest, StartSessionResponse, WorkflowAutoContinueResponse,
    ActiveSessionInfo,
};
use crate::database::CheckpointDb;
use crate::session::{Session, SessionConfig, SessionStatus};
use crate::settings;

/// Emit AI output to frontend
pub fn emit_ai_output(
    app_handle: &tauri::AppHandle,
    line: &str,
    source: &str,
    action_id: Option<&str>,
    session_ctx: Option<&AiOutputSessionContext>,
) {
    crate::mcp_api::emit_ai_output(app_handle, line, source, action_id, session_ctx)
}

/// List all unified sessions
pub async fn list_sessions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let sessions = state.session.list_sessions().await;
    Ok(Json(ApiResponse::success(sessions)))
}

/// Get a specific session
pub async fn get_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Session>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.session.get_session(&session_id).await {
        Some(session) => Ok(Json(ApiResponse::success(session))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Session {} not found", session_id))),
        )),
    }
}

/// Start a new unified session
pub async fn start_session(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartSessionRequest>,
) -> Result<Json<ApiResponse<StartSessionResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Delegate to mcp_api for the complex logic
    crate::mcp_api::start_session_handler(state, request).await
}

/// Stop a unified session
pub async fn stop_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Option<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session = state
        .session
        .stop_session(&session_id, "Stopped by user")
        .await;
    Ok(Json(ApiResponse::success(session)))
}

/// Delete a unified session
pub async fn delete_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    state.session.remove_session(&session_id).await;
    Ok(Json(ApiResponse::success(())))
}

/// Stop AI analysis
pub async fn stop_ai_analysis(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::stop_ai_analysis_handler(state).await
}

/// Get resumable workflow info
pub async fn get_resumable_workflow(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumableWorkflowInfo>> {
    crate::mcp_api::get_resumable_workflow_handler(state).await
}

/// Resume a workflow
pub async fn resume_workflow(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumeWorkflowResponse>> {
    crate::mcp_api::resume_workflow_handler(state).await
}

/// Force continue a session
pub async fn force_continue_session(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ForceContinueRequest>,
) -> Json<ApiResponse<ForceContinueResponse>> {
    crate::mcp_api::force_continue_session_handler(state, request).await
}

/// Get auto-continue setting
pub async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse { enabled }))
}

/// Set auto-continue setting
pub async fn set_auto_continue_setting(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<AutoContinueSettingResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!(
                "Auto-continue AI workflow setting updated to: {}",
                body.enabled
            );
            Json(ApiResponse::success(AutoContinueSettingResponse {
                enabled: body.enabled,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to save setting: {}", e)),
        }),
    }
}

/// Get workflow auto-continue setting
pub async fn get_workflow_auto_continue() -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();

    let workflow_name = if let Ok(db) = CheckpointDb::new() {
        db.get_running_task_runs()
            .ok()
            .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
    } else {
        None
    };

    Json(ApiResponse::success(WorkflowAutoContinueResponse {
        enabled,
        workflow_name,
    }))
}

/// Set workflow auto-continue setting
pub async fn set_workflow_auto_continue(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!("Auto-continue setting updated to: {}", body.enabled);

            let workflow_name = if let Ok(db) = CheckpointDb::new() {
                db.get_running_task_runs()
                    .ok()
                    .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
            } else {
                None
            };

            Json(ApiResponse::success(WorkflowAutoContinueResponse {
                enabled: body.enabled,
                workflow_name,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to update auto-continue setting: {}", e)),
        }),
    }
}

/// Resume all running tasks on startup
pub async fn resume_all_running_tasks_on_startup(state: Arc<ApiState>) -> usize {
    crate::mcp_api::resume_all_running_tasks_on_startup(state).await
}
