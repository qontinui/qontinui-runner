//! Scheduler handlers for MCP API
//!
//! Provides HTTP handlers for managing scheduled tasks:
//! CRUD operations, run-now, history, and settings.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

/// Request body for creating a scheduled task
#[derive(Debug, Deserialize)]
pub struct CreateScheduledTaskRequest {
    pub name: String,
    pub description: Option<String>,
    pub schedule: crate::scheduler::ScheduleExpression,
    pub task: crate::scheduler::ScheduledTaskType,
    #[serde(default)]
    pub skip_if_completed: bool,
    #[serde(default)]
    pub auto_fix_on_failure: bool,
    pub success_criteria: Option<String>,
    pub conditions: Option<crate::scheduler::ScheduleConditions>,
}

/// Request body for updating a scheduled task
#[derive(Debug, Deserialize)]
pub struct UpdateScheduledTaskRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub schedule: Option<crate::scheduler::ScheduleExpression>,
    pub task: Option<crate::scheduler::ScheduledTaskType>,
    pub skip_if_completed: Option<bool>,
    pub auto_fix_on_failure: Option<bool>,
    pub success_criteria: Option<Option<String>>,
    pub conditions: Option<Option<crate::scheduler::ScheduleConditions>>,
}

/// Request body for updating scheduler settings
#[derive(Debug, Deserialize)]
pub struct UpdateSchedulerSettingsRequest {
    pub enabled: Option<bool>,
    pub max_concurrent: Option<u32>,
    pub default_auto_fix_on_failure: Option<bool>,
    pub timezone: Option<Option<String>>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all scheduled tasks
pub async fn list_scheduled_tasks(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<crate::scheduler::ScheduledTask>>> {
    let tasks = crate::scheduler::get_all_tasks();
    Json(ApiResponse::success(tasks))
}

/// Create a new scheduled task
pub async fn create_scheduled_task(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateScheduledTaskRequest>,
) -> Result<
    (
        StatusCode,
        Json<ApiResponse<crate::scheduler::ScheduledTask>>,
    ),
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task = crate::scheduler::create_task(
        request.name,
        request.description,
        request.schedule,
        request.task,
        request.skip_if_completed,
        request.auto_fix_on_failure,
        request.success_criteria,
        request.conditions,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task: {}", e))),
        )
    })?;

    Ok((StatusCode::CREATED, Json(ApiResponse::success(task))))
}

/// Get a single scheduled task by ID
pub async fn get_scheduled_task(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::scheduler::ScheduledTask>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let task = crate::scheduler::get_task(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Task not found: {}", id))),
        )
    })?;

    Ok(Json(ApiResponse::success(task)))
}

/// Update an existing scheduled task
pub async fn update_scheduled_task(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ApiResponse<crate::scheduler::ScheduledTask>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let task = crate::scheduler::update_task(
        &id,
        request.name,
        request.description,
        request.enabled,
        request.schedule,
        request.task,
        request.skip_if_completed,
        request.auto_fix_on_failure,
        request.success_criteria,
        request.conditions,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update task: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(task)))
}

/// Delete a scheduled task
pub async fn delete_scheduled_task(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::scheduler::delete_task(&id).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Failed to delete task: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(())))
}

/// Run a scheduled task immediately (outside its schedule)
pub async fn run_task_now(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::scheduler_service::run_task_now(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to run task: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(())))
}

/// Get execution history for a task
pub async fn get_task_history(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<Vec<crate::scheduler::TaskExecutionRecord>>> {
    let history = crate::scheduler::get_task_history(&id);
    Json(ApiResponse::success(history))
}

/// Get scheduler settings
pub async fn get_scheduler_settings(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<crate::scheduler::SchedulerSettings>> {
    let settings = crate::scheduler::get_scheduler_settings();
    Json(ApiResponse::success(settings))
}

/// Update scheduler settings
pub async fn update_scheduler_settings(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<UpdateSchedulerSettingsRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load current settings and apply partial update
    let mut settings = crate::scheduler::get_scheduler_settings();

    if let Some(enabled) = request.enabled {
        settings.enabled = enabled;
    }
    if let Some(max_concurrent) = request.max_concurrent {
        settings.max_concurrent = max_concurrent;
    }
    if let Some(default_auto_fix) = request.default_auto_fix_on_failure {
        settings.default_auto_fix_on_failure = default_auto_fix;
    }
    if let Some(timezone) = request.timezone {
        settings.timezone = timezone;
    }

    crate::scheduler::update_scheduler_settings(settings).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update settings: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(())))
}

/// Get current scheduler status
pub async fn get_scheduler_status(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<crate::scheduler::SchedulerStatus>> {
    let status = crate::scheduler::get_scheduler_status();
    Json(ApiResponse::success(status))
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for the scheduler module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/scheduler/tasks",
            get(list_scheduled_tasks).post(create_scheduled_task),
        )
        .route(
            "/scheduler/tasks/:id",
            get(get_scheduled_task)
                .put(update_scheduled_task)
                .delete(delete_scheduled_task),
        )
        .route("/scheduler/tasks/:id/run", post(run_task_now))
        .route("/scheduler/tasks/:id/history", get(get_task_history))
        .route(
            "/scheduler/settings",
            get(get_scheduler_settings).put(update_scheduler_settings),
        )
        .route("/scheduler/status", get(get_scheduler_status))
}
