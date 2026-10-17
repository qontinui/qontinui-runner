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

use crate::mcp::origin_guard::{OriginClass, RequesterPrincipal};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::scheduler::{ScheduledTaskExt, TaskExecutionRecordExt};

// ============================================================================
// Types
// ============================================================================

/// Request body for creating a scheduled task
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateScheduledTaskRequest {
    pub name: String,
    pub description: Option<String>,
    pub schedule: crate::scheduler::ScheduleExpression,
    pub task: crate::scheduler::ScheduledTaskType,
    #[serde(default, alias = "skip_if_completed")]
    pub skip_if_completed: bool,
    #[serde(default, alias = "auto_fix_on_failure")]
    pub auto_fix_on_failure: bool,
    #[serde(alias = "success_criteria")]
    pub success_criteria: Option<String>,
    pub conditions: Option<crate::scheduler::ScheduleConditions>,
    #[serde(default, alias = "catch_up_policy")]
    pub catch_up_policy: Option<crate::scheduler::CatchUpPolicy>,
    #[serde(default, alias = "catch_up_grace_seconds")]
    pub catch_up_grace_seconds: Option<u32>,
    #[serde(default, alias = "launch_failure_backoff_seconds")]
    pub launch_failure_backoff_seconds: Option<u32>,
}

/// Request body for updating a scheduled task
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateScheduledTaskRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub schedule: Option<crate::scheduler::ScheduleExpression>,
    pub task: Option<crate::scheduler::ScheduledTaskType>,
    #[serde(default, alias = "skip_if_completed")]
    pub skip_if_completed: Option<bool>,
    #[serde(default, alias = "auto_fix_on_failure")]
    pub auto_fix_on_failure: Option<bool>,
    #[serde(default, alias = "success_criteria")]
    pub success_criteria: Option<Option<String>>,
    /// Absent = leave the conditions alone; `null` = clear them (also how a
    /// row flagged `conditionsError` is repaired to "no conditions"); an
    /// object = replace them. Deserialized explicitly for the same reason as
    /// `timezone`: serde folds `null` into the outer `None` by default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub conditions: Option<Option<crate::scheduler::ScheduleConditions>>,
    #[serde(default, alias = "catch_up_policy")]
    pub catch_up_policy: Option<crate::scheduler::CatchUpPolicy>,
    #[serde(default, alias = "catch_up_grace_seconds")]
    pub catch_up_grace_seconds: Option<u32>,
    #[serde(default, alias = "launch_failure_backoff_seconds")]
    pub launch_failure_backoff_seconds: Option<u32>,
}

/// Request body for updating scheduler settings
#[derive(Debug, Deserialize)]
pub struct UpdateSchedulerSettingsRequest {
    pub enabled: Option<bool>,
    pub max_concurrent: Option<u32>,
    pub default_auto_fix_on_failure: Option<bool>,
    /// Absent = leave the zone alone; `null` = clear it back to local time;
    /// a string = set it. Serde folds `null` into the outer `None` by default,
    /// which made the clear arm unreachable from JSON — so the field is
    /// deserialized explicitly (Phase 5a made the zone load-bearing).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub timezone: Option<Option<String>>,
}

/// Three-state field: `"field": null` -> `Some(None)`; `"field": value` ->
/// `Some(Some(value))`; absent -> `None` (via `#[serde(default)]`, which must
/// accompany it — without it an absent field is an error).
fn deserialize_double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// A failed scheduler read. It is answered as a 500 rather than as an empty
/// list or default settings, so a caller can tell "none" from "unknown".
type ReadError = (StatusCode, Json<ApiResponse<()>>);

fn read_failed(what: &str, e: impl std::fmt::Display) -> ReadError {
    tracing::error!("Failed to read {}: {}", what, e);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(api_error(format!("Failed to read {}: {}", what, e))),
    )
}

// ============================================================================
// requireProbe authorization
// ============================================================================

/// Refuse a `requireProbe` the requester may not set.
///
/// A probe is a command the scheduler execs as this device's user, so writing
/// one is command execution. `POST /scheduler/tasks` and
/// `PUT /scheduler/tasks/{id}` are `TRUSTED_ROUTES` (the qontinui-web dev
/// frontend manages tasks through them), and under the default
/// `EnforceDoors` route policy a Foreign browser origin reaching them is only
/// SHADOWED, i.e. admitted. Moving the two routes to `CREDENTIAL_DOORS` would
/// cut the web frontend off from task management altogether, so the gate is
/// narrower and lives here: only a NonBrowser caller (an agent, a script,
/// `curl`) or the runner's own webview (FirstParty) may set or change a
/// probe. Every browser class — Trusted included, because an operator-added
/// Trusted origin gets every other command-execution door refused — is
/// answered 403. An absent principal (a request that did not pass the origin
/// guard) is UNKNOWN and refused, never read as trusted.
///
/// `existing` is the stored probe on an update: re-sending it unchanged (a web
/// UI echoing a task back while renaming it) grants nothing new and passes.
/// `Err` carries the refusal message; the handlers answer it with 403.
pub(crate) fn refuse_untrusted_probe(
    principal: Option<&RequesterPrincipal>,
    requested: Option<&crate::scheduler::ProbeCondition>,
    existing: Option<&crate::scheduler::ProbeCondition>,
) -> Result<(), String> {
    let Some(requested) = requested else {
        return Ok(());
    };
    if existing == Some(requested) {
        return Ok(());
    }
    match principal.map(|p| p.class) {
        Some(OriginClass::NonBrowser | OriginClass::FirstParty) => Ok(()),
        class => Err(format!(
            "requireProbe runs a command as this device's user; only a non-browser caller \
             or the runner's own UI may set or change it (requester class: {})",
            class.map(OriginClass::as_str).unwrap_or("unknown")
        )),
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// A stored task as the API serves it: the task, plus a `conditionsError`
/// string when its stored conditions are unreadable (then `conditions` is
/// null, and the scheduler refuses to run the task until a PUT that carries
/// `conditions` replaces them).
pub(crate) fn stored_task_json(
    stored: crate::database::pg::scheduler::StoredScheduledTask,
) -> serde_json::Value {
    let mut value = serde_json::to_value(&stored.task).unwrap_or_else(
        |e| serde_json::json!({ "id": stored.task.id, "serializeError": e.to_string() }),
    );
    if let (Some(error), Some(object)) = (stored.conditions_error, value.as_object_mut()) {
        object.insert(
            "conditionsError".to_string(),
            serde_json::Value::String(error),
        );
    }
    value
}

/// List all scheduled tasks — every stored row, including one whose stored
/// conditions are unreadable (flagged with `conditionsError`, so it can be
/// seen and repaired; the scheduler does not run it).
pub async fn list_scheduled_tasks(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, ReadError> {
    let pg = &state.app_state.pg_db;
    let tasks = pg
        .get_all_stored_scheduled_tasks()
        .await
        .map_err(|e| read_failed("scheduled tasks", e))?;
    Ok(Json(ApiResponse::success(
        tasks.into_iter().map(stored_task_json).collect(),
    )))
}

/// Create a new scheduled task
pub async fn create_scheduled_task(
    State(state): State<Arc<ApiState>>,
    principal: Option<axum::Extension<RequesterPrincipal>>,
    Json(request): Json<CreateScheduledTaskRequest>,
) -> Result<
    (
        StatusCode,
        Json<ApiResponse<crate::scheduler::ScheduledTask>>,
    ),
    (StatusCode, Json<ApiResponse<()>>),
> {
    let pg = &state.app_state.pg_db;

    if let Some(conditions) = &request.conditions {
        refuse_untrusted_probe(
            principal.as_ref().map(|p| &p.0),
            conditions.require_probe.as_ref(),
            None,
        )
        .map_err(|e| (StatusCode::FORBIDDEN, Json(api_error(e))))?;
        crate::scheduler_probe::validate_conditions(conditions)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    }

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
    if let Some(policy) = request.catch_up_policy {
        scheduled_task.catch_up_policy = policy;
    }
    if let Some(grace) = request.catch_up_grace_seconds {
        scheduled_task.catch_up_grace_seconds = grace;
    }
    if let Some(backoff) = request.launch_failure_backoff_seconds {
        scheduled_task.launch_failure_backoff_seconds = backoff;
    }

    let now = chrono::Utc::now();
    let zone = crate::scheduler::ScheduleZone::from_settings(
        &pg.get_scheduler_settings().await.unwrap_or_default(),
    );
    scheduled_task.next_run =
        crate::scheduler::compute_next_run(&scheduled_task.schedule, now, zone)
            .map(|dt| dt.to_rfc3339());

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

/// Get a single scheduled task by ID (flagged with `conditionsError`, not
/// refused, when its stored conditions are unreadable).
pub async fn get_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let task = pg
        .get_stored_scheduled_task(&id)
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

    Ok(Json(ApiResponse::success(stored_task_json(task))))
}

/// Update an existing scheduled task
pub async fn update_scheduled_task(
    State(state): State<Arc<ApiState>>,
    principal: Option<axum::Extension<RequesterPrincipal>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    // The STORED row, flagged rather than refused when its conditions are
    // unreadable: a PUT that carries `conditions` is how such a row is
    // repaired (the stored value is replaced, never parsed). A PUT without
    // `conditions` leaves the stored conditions untouched in the database
    // (`write_conditions` below), so it can neither erase them to "ungated"
    // nor revert a newer value.
    let stored = pg
        .get_stored_scheduled_task(&id)
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
    let conditions_error = stored.conditions_error;
    let mut scheduled_task = stored.task;
    let write_conditions = request.conditions.is_some();

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
        if let Some(conditions) = &conditions {
            refuse_untrusted_probe(
                principal.as_ref().map(|p| &p.0),
                conditions.require_probe.as_ref(),
                scheduled_task
                    .conditions
                    .as_ref()
                    .and_then(|c| c.require_probe.as_ref()),
            )
            .map_err(|e| (StatusCode::FORBIDDEN, Json(api_error(e))))?;
            crate::scheduler_probe::validate_conditions(conditions)
                .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
        }
        scheduled_task.conditions = conditions;
        scheduled_task.condition_status = None;
    }
    if let Some(policy) = request.catch_up_policy {
        scheduled_task.catch_up_policy = policy;
    }
    if let Some(grace) = request.catch_up_grace_seconds {
        scheduled_task.catch_up_grace_seconds = grace;
    }
    if let Some(backoff) = request.launch_failure_backoff_seconds {
        scheduled_task.launch_failure_backoff_seconds = backoff;
    }

    let now = chrono::Utc::now();
    let zone = crate::scheduler::ScheduleZone::from_settings(
        &pg.get_scheduler_settings().await.unwrap_or_default(),
    );
    scheduled_task.next_run = if scheduled_task.enabled {
        crate::scheduler::compute_next_run(&scheduled_task.schedule, now, zone)
            .map(|dt| dt.to_rfc3339())
    } else {
        None
    };

    // Conditional write against the snapshot every check above was made on
    // (the requireProbe echo exemption included): if the row moved since it
    // was read, nothing is written and the caller gets 409 — a stale
    // snapshot can never revert a newer change.
    let read_modified_at = scheduled_task.modified_at.clone();
    scheduled_task.touch();
    let written = pg
        .update_scheduled_task(&scheduled_task, &read_modified_at, write_conditions)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update task: {}", e))),
            )
        })?;
    if !written {
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "task {id} changed while this update was being applied; re-read it and retry"
            ))),
        ));
    }

    tracing::info!(
        "Updated scheduled task: {} ({})",
        scheduled_task.name,
        scheduled_task.id
    );
    Ok(Json(ApiResponse::success(stored_task_json(
        crate::database::pg::scheduler::StoredScheduledTask {
            task: scheduled_task,
            // Replaced by this PUT, or still stored (and still unreadable).
            conditions_error: if write_conditions {
                None
            } else {
                conditions_error
            },
        },
    ))))
}

/// Delete a scheduled task
pub async fn delete_scheduled_task(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    // Verify task exists first — as STORED: a row flagged `conditionsError`
    // must stay deletable (the runnable read refuses it).
    pg.get_stored_scheduled_task(&id)
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
    crate::scheduler_service::run_task_now(&id, crate::coord_drain_state::SpawnOrigin::Unknown)
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
) -> Result<Json<ApiResponse<Vec<crate::scheduler::TaskExecutionRecord>>>, ReadError> {
    let pg = &state.app_state.pg_db;
    let history = pg
        .get_execution_history(&id, 50)
        .await
        .map_err(|e| read_failed(&format!("task history for {id}"), e))?;
    Ok(Json(ApiResponse::success(history)))
}

/// Get scheduler settings
pub async fn get_scheduler_settings(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<crate::scheduler::SchedulerSettings>>, ReadError> {
    let pg = &state.app_state.pg_db;
    let settings = pg
        .get_scheduler_settings()
        .await
        .map_err(|e| read_failed("scheduler settings", e))?;
    Ok(Json(ApiResponse::success(settings)))
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

    let previous_timezone = settings.timezone.clone();
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
        // Phase 5a: the zone is now READ (every cron site evaluates in it), so
        // a value that does not parse is refused here rather than stored and
        // silently ignored — `None` (local time) is always accepted.
        if let Some(name) = timezone.as_deref() {
            if let Err(e) = crate::scheduler::ScheduleZone::parse(name) {
                return Err((StatusCode::BAD_REQUEST, Json(api_error(e))));
            }
        }
        settings.timezone = timezone;
    }
    let zone_changed = settings.timezone != previous_timezone;

    pg.update_scheduler_settings(&settings).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update settings: {}", e))),
        )
    })?;

    // The tick fires off each task's STORED `next_run`, so a zone change must
    // recompute them now or every enabled task honours the old zone once more
    // (on a UTC+2 box: one more 06:20 firing — the defect 5a removes).
    if zone_changed {
        let zone = crate::scheduler::ScheduleZone::from_settings(&settings);
        let now = chrono::Utc::now();
        match pg.get_all_scheduled_tasks().await {
            Ok(tasks) => {
                for task in tasks.iter().filter(|t| t.enabled) {
                    let next = crate::scheduler::compute_next_run(&task.schedule, now, zone)
                        .map(|dt| dt.to_rfc3339());
                    match pg
                        .update_task_next_run(&task.id, next.as_deref(), &task.modified_at)
                        .await
                    {
                        Ok(true) => {}
                        // Edited since this read; the edit recomputed next_run
                        // in the new zone itself.
                        Ok(false) => {}
                        Err(e) => tracing::warn!(
                            "scheduler: timezone changed but next_run for task {} was not recomputed: {e}",
                            task.id
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                "scheduler: timezone changed but the task list could not be read for a next_run recompute: {e}"
            ),
        }
    }

    Ok(Json(ApiResponse::success(())))
}

/// Get current scheduler status
pub async fn get_scheduler_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<crate::scheduler::SchedulerStatus>>, ReadError> {
    let pg = &state.app_state.pg_db;
    let tasks = pg
        .get_all_scheduled_tasks()
        .await
        .map_err(|e| read_failed("scheduled tasks", e))?;
    let settings = pg
        .get_scheduler_settings()
        .await
        .map_err(|e| read_failed("scheduler settings", e))?;

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
    Ok(Json(ApiResponse::success(status)))
}

/// Manually trigger the missed-run reconciler. Normally `reconcile_missed_runs`
/// runs once at scheduler startup; this endpoint lets operators / tests fire it
/// on demand without restarting the runner. Idempotent — running it twice in a
/// row does nothing the second time, since the first pass already filled any
/// gaps in `scheduler_history.scheduled_for`.
pub async fn reconcile_now(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let service = crate::scheduler_service::get_scheduler_service()
        .await
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(api_error(
                    "Scheduler service not running on this instance".to_string(),
                )),
            )
        })?;

    let ran = service
        .clone()
        .try_reconcile_missed_runs()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Reconciler failed: {}", e))),
            )
        })?;
    if !ran {
        // A tick (which may be running a sync task to completion) or another
        // reconcile holds the pass lock; answer now rather than hang behind it.
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(
                "a scheduler pass is in progress; retry shortly".to_string(),
            )),
        ));
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "triggered": true,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))))
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
        .route("/scheduler/reconcile-now", post(reconcile_now))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 5a: the three shapes of `timezone` on a settings PUT are three
    /// different requests, and `null` (clear back to local time) must not fold
    /// into "leave it alone".
    #[test]
    fn timezone_absent_null_and_string_are_three_different_requests() {
        let absent: UpdateSchedulerSettingsRequest =
            serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(absent.timezone, None);
        let null: UpdateSchedulerSettingsRequest =
            serde_json::from_str(r#"{"timezone":null}"#).unwrap();
        assert_eq!(null.timezone, Some(None));
        let set: UpdateSchedulerSettingsRequest =
            serde_json::from_str(r#"{"timezone":"Europe/Berlin"}"#).unwrap();
        assert_eq!(set.timezone, Some(Some("Europe/Berlin".to_string())));
    }

    fn probe(cmd: &str) -> crate::scheduler::ProbeCondition {
        crate::scheduler::ProbeCondition {
            enabled: true,
            command: vec!["sh".into(), "-c".into(), cmd.into()],
            poll_seconds: 60,
            timeout_seconds: 10,
        }
    }

    fn principal(class: OriginClass) -> RequesterPrincipal {
        RequesterPrincipal {
            class,
            origin: None,
        }
    }

    #[test]
    fn only_non_browser_and_first_party_may_set_a_probe() {
        let p = probe("exit 0");
        for class in [OriginClass::NonBrowser, OriginClass::FirstParty] {
            assert!(refuse_untrusted_probe(Some(&principal(class)), Some(&p), None).is_ok());
        }
        for class in [
            OriginClass::Trusted,
            OriginClass::Extension,
            OriginClass::Foreign,
        ] {
            let refusal =
                refuse_untrusted_probe(Some(&principal(class)), Some(&p), None).unwrap_err();
            assert!(refusal.contains(class.as_str()), "{refusal}");
        }
        // No principal = UNKNOWN, refused.
        assert!(refuse_untrusted_probe(None, Some(&p), None)
            .unwrap_err()
            .contains("unknown"));
        // No probe in the request: nothing to gate.
        assert!(refuse_untrusted_probe(Some(&principal(OriginClass::Foreign)), None, None).is_ok());
    }

    #[test]
    fn echoing_the_stored_probe_back_unchanged_is_not_a_new_grant() {
        let stored = probe("exit 0");
        let foreign = principal(OriginClass::Trusted);
        assert!(refuse_untrusted_probe(Some(&foreign), Some(&stored), Some(&stored)).is_ok());
        let changed = probe("id > /tmp/pwned");
        assert!(refuse_untrusted_probe(Some(&foreign), Some(&changed), Some(&stored)).is_err());
        let mut enabled = stored.clone();
        enabled.enabled = false;
        assert!(
            refuse_untrusted_probe(Some(&foreign), Some(&stored), Some(&enabled)).is_err(),
            "enabling a stored probe is a change"
        );
    }

    /// Both write handlers gate the probe on the requester BEFORE persisting.
    #[test]
    fn both_task_write_handlers_gate_the_probe_before_persisting() {
        // The write needles omit the `pg` receiver: rustfmt wraps the update
        // call as `pg\n        .update_scheduled_task(`, so `pg.update_…(`
        // matched only this test's own literal while it scanned its own file.
        let src = crate::source_pin::ProdSource::of(include_str!("scheduler.rs"));
        for (handler, write) in [
            (
                "pub async fn create_scheduled_task(",
                ".insert_scheduled_task(",
            ),
            (
                "pub async fn update_scheduled_task(",
                ".update_scheduled_task(",
            ),
        ] {
            let (_, body) = src.split_once(handler).expect(handler);
            let (before_gate, _) = body
                .split_once("refuse_untrusted_probe(")
                .expect("gate call");
            let (before_persist, _) = body.split_once(write).expect("persist call");
            assert!(
                before_gate.len() < before_persist.len(),
                "{handler} must refuse before {write}"
            );
            assert!(before_gate.contains("Option<axum::Extension<RequesterPrincipal>>"));
        }
    }

    /// The update is a conditional write keyed on the `modified_at` the
    /// handler read, and a lost race is a 409 (review round 2: the echo
    /// exemption was a read-then-write TOCTOU).
    #[test]
    fn the_update_is_a_conditional_write_on_the_modified_at_it_read() {
        let src = crate::source_pin::ProdSource::of(include_str!("scheduler.rs"));
        let (_, body) = src
            .split_once("pub async fn update_scheduled_task(")
            .expect("update handler");
        let (body, _) = body
            .split_once("pub async fn delete_scheduled_task(")
            .expect("update handler ends");
        let (before_touch, _) = body.split_once("scheduled_task.touch();").expect("touch");
        assert!(before_touch.contains("let read_modified_at = scheduled_task.modified_at.clone();"));
        assert!(body.contains(
            ".update_scheduled_task(&scheduled_task, &read_modified_at, write_conditions)"
        ));
        assert!(body.contains("StatusCode::CONFLICT"));

        let pg = include_str!("../database/pg/scheduler.rs");
        assert!(pg.contains("WHERE id = $15 AND modified_at = $16"));
        assert!(pg.contains("Ok(updated == 1)"));
    }

    /// `/scheduler/reconcile-now` does not wait behind a pass in progress; the
    /// wake handler skips instead of waiting.
    #[test]
    fn reconcile_now_and_the_wake_handler_never_wait_behind_a_pass() {
        let src = crate::source_pin::ProdSource::of(include_str!("scheduler.rs"));
        let (_, body) = src
            .split_once("pub async fn reconcile_now(")
            .expect("reconcile_now");
        let (body, _) = body.split_once("\n}\n").expect("reconcile_now ends");
        assert!(body.contains(".try_reconcile_missed_runs()"));
        assert!(!body.contains(".reconcile_missed_runs()"));
        assert!(body.contains("StatusCode::CONFLICT"));

        let wake = include_str!("../wake_handler.rs");
        assert!(wake.contains("service.try_tick().await"));
        assert!(!wake.contains("service.tick().await"));
    }

    /// A row with unreadable conditions is served, flagged: `conditions`
    /// null plus a `conditionsError` string.
    #[test]
    fn a_row_with_unreadable_conditions_is_served_flagged() {
        use crate::scheduler::ScheduledTaskExt;
        let task = crate::scheduler::ScheduledTask::new(
            "t".to_string(),
            None,
            crate::scheduler::ScheduleExpression::Cron("0 0 * * * *".to_string()),
            crate::scheduler::scheduled_task_type_default(),
        );
        let flagged = stored_task_json(crate::database::pg::scheduler::StoredScheduledTask {
            task: task.clone(),
            conditions_error: Some("unreadable".to_string()),
        });
        assert_eq!(flagged["conditionsError"], "unreadable");
        assert!(flagged.get("conditions").is_none_or(|c| c.is_null()));
        assert_eq!(flagged["id"], task.id.as_str());
        let clean = stored_task_json(crate::database::pg::scheduler::StoredScheduledTask {
            task,
            conditions_error: None,
        });
        assert!(clean.get("conditionsError").is_none());
    }

    /// list/get/update read the STORED (flagged) row; update writes
    /// conditions only when the request carried them.
    #[test]
    fn the_task_api_reads_stored_rows_and_writes_conditions_only_when_sent() {
        let src = crate::source_pin::ProdSource::of(include_str!("scheduler.rs"));
        for handler in [
            "pub async fn list_scheduled_tasks(",
            "pub async fn get_scheduled_task(",
            "pub async fn update_scheduled_task(",
        ] {
            let (_, body) = src.split_once(handler).expect(handler);
            let (body, _) = body.split_once("\n}\n").expect("handler ends");
            assert!(
                body.contains("get_all_stored_scheduled_tasks()")
                    || body.contains("get_stored_scheduled_task(&id)"),
                "{handler} must read the stored row"
            );
        }
        let (_, update) = src
            .split_once("pub async fn update_scheduled_task(")
            .unwrap();
        assert!(update.contains("let write_conditions = request.conditions.is_some();"));
        assert!(update.contains("&read_modified_at, write_conditions)"));
    }

    /// `conditions` on an update is three-state: absent leaves them, `null`
    /// clears them (and repairs a flagged row), an object replaces them.
    #[test]
    fn update_conditions_absent_null_and_object_are_three_different_requests() {
        let absent: UpdateScheduledTaskRequest = serde_json::from_str(r#"{"name":"n"}"#).unwrap();
        assert!(absent.conditions.is_none());
        let null: UpdateScheduledTaskRequest =
            serde_json::from_str(r#"{"conditions":null}"#).unwrap();
        assert!(matches!(null.conditions, Some(None)));
        let object: UpdateScheduledTaskRequest =
            serde_json::from_str(r#"{"conditions":{"timeoutMinutes":30}}"#).unwrap();
        assert!(matches!(
            object.conditions,
            Some(Some(ref c)) if c.timeout_minutes == Some(30)
        ));
    }

    /// Delete checks existence against the STORED row, so a flagged row is
    /// deletable rather than a 500.
    #[test]
    fn delete_checks_existence_against_the_stored_row() {
        let src = crate::source_pin::ProdSource::of(include_str!("scheduler.rs"));
        let (_, body) = src
            .split_once("pub async fn delete_scheduled_task(")
            .expect("delete handler");
        let (body, _) = body.split_once("\n}\n").expect("delete handler ends");
        assert!(body.contains("pg.get_stored_scheduled_task(&id)"));
        assert!(!body.contains("pg.get_scheduled_task(&id)"));
    }
}
