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
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<crate::scheduler::ScheduledTask>>> {
    let pg = &state.app_state.pg_db;
    match pg.get_all_scheduled_tasks().await {
        Ok(tasks) => Json(ApiResponse::success(tasks)),
        Err(e) => {
            tracing::error!("Failed to get all scheduled tasks: {}", e);
            Json(ApiResponse::success(Vec::new()))
        }
    }
}

/// Create a new scheduled task
pub async fn create_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateScheduledTaskRequest>,
) -> Result<
    (
        StatusCode,
        Json<ApiResponse<crate::scheduler::ScheduledTask>>,
    ),
    (StatusCode, Json<ApiResponse<()>>),
> {
    let pg = &state.app_state.pg_db;

    let mut scheduled_task = crate::scheduler::ScheduledTask::new(
        request.name,
        request.description,
        request.schedule,
        request.task,
    );
    scheduled_task.skip_if_completed = request.skip_if_completed;
    scheduled_task.auto_fix_on_failure = request.auto_fix_on_failure;
    scheduled_task.success_criteria = request.success_criteria;
    scheduled_task.conditions = request.conditions;

    let now = chrono::Utc::now();
    scheduled_task.next_run =
        crate::scheduler::compute_next_run(&scheduled_task.schedule, now).map(|dt| dt.to_rfc3339());

    pg.insert_scheduled_task(&scheduled_task)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create task: {}", e))),
            )
        })?;

    tracing::info!(
        "Created scheduled task: {} ({})",
        scheduled_task.name,
        scheduled_task.id
    );
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::success(scheduled_task)),
    ))
}

/// Get a single scheduled task by ID
pub async fn get_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::scheduler::ScheduledTask>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;
    let task = pg
        .get_scheduled_task(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get task: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Task not found: {}", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(task)))
}

/// Update an existing scheduled task
pub async fn update_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ApiResponse<crate::scheduler::ScheduledTask>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;
    let mut scheduled_task = pg
        .get_scheduled_task(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get task: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Task not found: {}", id))),
            )
        })?;

    if let Some(name) = request.name {
        scheduled_task.name = name;
    }
    if let Some(description) = request.description {
        scheduled_task.description = description;
    }
    if let Some(enabled) = request.enabled {
        scheduled_task.enabled = enabled;
    }
    if let Some(schedule) = request.schedule {
        scheduled_task.schedule = schedule;
    }
    if let Some(task) = request.task {
        scheduled_task.task = task;
    }
    if let Some(skip_if_completed) = request.skip_if_completed {
        scheduled_task.skip_if_completed = skip_if_completed;
    }
    if let Some(auto_fix_on_failure) = request.auto_fix_on_failure {
        scheduled_task.auto_fix_on_failure = auto_fix_on_failure;
    }
    if let Some(success_criteria) = request.success_criteria {
        scheduled_task.success_criteria = success_criteria;
    }
    if let Some(conditions) = request.conditions {
        scheduled_task.conditions = conditions;
        scheduled_task.condition_status = None;
    }

    let now = chrono::Utc::now();
    scheduled_task.next_run = if scheduled_task.enabled {
        crate::scheduler::compute_next_run(&scheduled_task.schedule, now).map(|dt| dt.to_rfc3339())
    } else {
        None
    };

    scheduled_task.touch();
    pg.update_scheduled_task(&scheduled_task)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update task: {}", e))),
            )
        })?;

    tracing::info!(
        "Updated scheduled task: {} ({})",
        scheduled_task.name,
        scheduled_task.id
    );
    Ok(Json(ApiResponse::success(scheduled_task)))
}

/// Delete a scheduled task
pub async fn delete_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    // Verify task exists first
    pg.get_scheduled_task(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get task: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Task not found: {}", id))),
            )
        })?;

    pg.delete_scheduled_task(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete task: {}", e))),
        )
    })?;
    tracing::info!("Deleted scheduled task: {}", id);

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
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<Vec<crate::scheduler::TaskExecutionRecord>>> {
    let pg = &state.app_state.pg_db;
    match pg.get_execution_history(&id, 50).await {
        Ok(history) => Json(ApiResponse::success(history)),
        Err(e) => {
            tracing::error!("Failed to get task history for {}: {}", id, e);
            Json(ApiResponse::success(Vec::new()))
        }
    }
}

/// Get scheduler settings
pub async fn get_scheduler_settings(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<crate::scheduler::SchedulerSettings>> {
    let pg = &state.app_state.pg_db;
    match pg.get_scheduler_settings().await {
        Ok(settings) => Json(ApiResponse::success(settings)),
        Err(e) => {
            tracing::error!("Failed to get scheduler settings: {}", e);
            Json(ApiResponse::success(
                crate::scheduler::SchedulerSettings::default(),
            ))
        }
    }
}

/// Update scheduler settings
pub async fn update_scheduler_settings(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UpdateSchedulerSettingsRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    // Load current settings and apply partial update
    let mut settings = pg.get_scheduler_settings().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get settings: {}", e))),
        )
    })?;

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

    pg.update_scheduler_settings(&settings).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update settings: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(())))
}

/// Get current scheduler status
pub async fn get_scheduler_status(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<crate::scheduler::SchedulerStatus>> {
    let pg = &state.app_state.pg_db;
    let tasks = pg.get_all_scheduled_tasks().await.unwrap_or_default();
    let settings = pg.get_scheduler_settings().await.unwrap_or_default();

    let running_tasks = tasks
        .iter()
        .filter(|t| {
            t.last_run
                .as_ref()
                .map(|r| r.status == crate::scheduler::ScheduledTaskStatus::Running)
                .unwrap_or(false)
        })
        .count() as u32;

    let pending_tasks = tasks.iter().filter(|t| t.enabled).count() as u32;

    let next_task = tasks
        .iter()
        .filter(|t| t.enabled && t.next_run.is_some())
        .min_by_key(|t| t.next_run.as_ref().unwrap())
        .map(|t| crate::scheduler::NextTaskInfo {
            id: t.id.clone(),
            name: t.name.clone(),
            next_run: t.next_run.clone().unwrap(),
        });

    let status = crate::scheduler::SchedulerStatus {
        enabled: settings.enabled,
        running_tasks,
        pending_tasks,
        next_task,
    };
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
            "/scheduler/tasks/{id}",
            get(get_scheduled_task)
                .put(update_scheduled_task)
                .delete(delete_scheduled_task),
        )
        .route("/scheduler/tasks/{id}/run", post(run_task_now))
        .route("/scheduler/tasks/{id}/history", get(get_task_history))
        .route(
            "/scheduler/settings",
            get(get_scheduler_settings).put(update_scheduler_settings),
        )
        .route("/scheduler/status", get(get_scheduler_status))
}
