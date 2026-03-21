//! Task run handlers for MCP API
//!
//! Provides HTTP handlers for task run management:
//! CRUD, workflow state, execution control, event queries,
//! verification results, knowledge, screenshots, and more.

use axum::{extract::State, http::StatusCode, response::sse::Sse, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::database::{CreateTaskRunInput, TaskRun};
use crate::mcp::shared::{emit_ai_output, AiSessionContext};
use crate::mcp::types::ApiState;
use crate::safe_lock::safe_lock_or_recover;
use crate::summary_generator;
use tauri::Manager;

// Re-export handlers from extracted submodules for backward compatibility
pub use super::task_run_queries::*;
pub use super::task_run_workflow_state::*;

/// Query params for listing task runs.
#[derive(Debug, Deserialize)]
pub struct ListTaskRunsQuery {
    /// Maximum number of task runs to return (default: 50)
    limit: Option<u32>,
    /// Filter by workflow_type (e.g., "plan", "unified", "automation_only")
    workflow_type: Option<String>,
}

/// List recent task runs.
/// Uses spawn_blocking to avoid blocking the async runtime on database operations.
pub async fn list_task_runs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ListTaskRunsQuery>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);
    let workflow_type = query.workflow_type;
    let db = state.app_state.checkpoint_db.clone();
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    tokio::task::spawn_blocking(move || {
        db.get_recent_task_runs_filtered(limit, workflow_type.as_deref(), Some(port))
    })
    .await
    .map_err(|e| {
        error!("spawn_blocking error in list_task_runs: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// List only running task runs.
/// Uses spawn_blocking to avoid blocking the async runtime on database operations.
pub async fn list_running_task_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    tokio::task::spawn_blocking(move || db.get_running_task_runs(Some(port)))
        .await
        .map_err(|e| {
            error!("spawn_blocking error in list_running_task_runs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for creating a task run.
#[derive(Debug, Deserialize)]
pub struct CreateTaskRunRequest {
    /// Name/identifier for this task
    task_name: String,
    /// The prompt to run (optional for pure automation tasks)
    #[serde(default)]
    prompt: Option<String>,
    /// Task type: 'task', 'automation', or 'scheduled' (defaults to 'task')
    #[serde(default)]
    task_type: Option<String>,
    /// Config ID for automation-enabled tasks
    #[serde(default)]
    config_id: Option<String>,
    /// Workflow name being executed
    #[serde(default)]
    workflow_name: Option<String>,
    /// Maximum number of sessions before giving up (optional)
    #[serde(default)]
    max_sessions: Option<u32>,
    /// Per-run auto-continue setting (defaults to true if not specified)
    #[serde(default)]
    auto_continue: Option<bool>,
    /// JSON-encoded execution steps (optional)
    #[serde(default)]
    execution_steps_json: Option<String>,
    /// JSON-encoded log sources (optional)
    #[serde(default)]
    log_sources_json: Option<String>,
}

/// Create a new task run.
pub async fn create_task_run(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateTaskRunRequest>,
) -> Result<Json<TaskRun>, (StatusCode, String)> {
    let id = uuid::Uuid::new_v4().to_string();
    let task_type = req.task_type.as_deref().unwrap_or("task");

    let mut input = CreateTaskRunInput::new(&id, &req.task_name).with_task_type(task_type);
    if let Some(ref p) = req.prompt {
        input = input.with_prompt(p);
    }
    if let Some(ref cid) = req.config_id {
        input = input.with_config_id(cid);
    }
    if let Some(ref wn) = req.workflow_name {
        input = input.with_workflow_name(wn);
    }
    if let Some(ms) = req.max_sessions {
        input = input.with_max_sessions(ms);
    }
    if let Some(ac) = req.auto_continue {
        input = input.with_auto_continue(ac);
    }
    if let Some(ref esj) = req.execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Some(ref lsj) = req.log_sources_json {
        input = input.with_log_sources_json(lsj);
    }

    state
        .app_state
        .checkpoint_db
        .create_task_run(&input)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get a task run by ID.
pub async fn get_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Option<TaskRun>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Response for workflow state endpoint.
pub async fn stop_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("Stopping task run: {}", id);

    // Verify task exists first
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    if task_run.status != "running" {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": format!("Task is not running (status: {})", task_run.status)
        })));
    }

    // Kill all tracked AI processes (same logic as stop_ai_analysis)
    // This ensures the actual Claude CLI process is terminated, not just marked as stopped
    let pids_to_kill: Vec<u32> = {
        let mut pids = safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        let pids_copy = pids.clone();
        pids.clear();
        pids_copy
    };

    let mut killed_count = 0;
    for pid in &pids_to_kill {
        info!("Killing AI process PID {} for task {}", pid, id);
        let result = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    info!("Successfully killed process tree for PID {}", pid);
                    killed_count += 1;
                } else {
                    // Process may have already exited
                    killed_count += 1;
                }
            }
            Err(e) => {
                error!("Failed to execute taskkill for PID {}: {}", pid, e);
            }
        }
    }

    // Mark as stopped in database
    state
        .app_state
        .checkpoint_db
        .stop_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Explicitly release URL locks for this task (don't rely solely on
    // WorkflowDropGuard's sync release, which can fail under contention)
    state.app_state.url_lock_manager.release_all(&id).await;

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🛑 Task {} stopped (killed {} process(es))",
            id, killed_count
        ),
        "status",
        None,
        None,
    );

    // Broadcast task-run-update to both Tauri + WebSocket
    let broadcaster = crate::event_system::EventBroadcaster::new(state.app_handle.clone());
    broadcaster.task_run_update(&id, "stopped", None, None);

    info!("Task {} stopped, killed {} process(es)", id, killed_count);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Task run stopped, killed {} process(es)", killed_count)
    })))
}

/// Pause a running task run.
///
/// Sets the task status to 'paused'. The loop controller checks this status
/// at the start of each iteration and between phases, waiting until the task
/// is unpaused before continuing.
pub async fn pause_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("Pausing task run: {}", id);

    let paused = state
        .app_state
        .checkpoint_db
        .pause_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if !paused {
        // Either task not found or not in 'running' status
        let task_run = state
            .app_state
            .checkpoint_db
            .get_task_run(&id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        return match task_run {
            None => Err((StatusCode::NOT_FOUND, format!("Task run not found: {}", id))),
            Some(tr) => Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("Task is not running (status: {})", tr.status)
            }))),
        };
    }

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!("⏸ Task {} paused", id),
        "status",
        None,
        None,
    );

    // Broadcast task-run-update
    let broadcaster = crate::event_system::EventBroadcaster::new(state.app_handle.clone());
    broadcaster.task_run_update(&id, "paused", None, None);

    info!("Task {} paused", id);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Task run paused"
    })))
}

/// Unpause (resume) a paused task run.
///
/// Sets the task status back to 'running'. The loop controller will detect
/// this change and continue execution.
pub async fn unpause_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("Unpausing task run: {}", id);

    let unpaused = state
        .app_state
        .checkpoint_db
        .unpause_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if !unpaused {
        let task_run = state
            .app_state
            .checkpoint_db
            .get_task_run(&id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        return match task_run {
            None => Err((StatusCode::NOT_FOUND, format!("Task run not found: {}", id))),
            Some(tr) => Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("Task is not paused (status: {})", tr.status)
            }))),
        };
    }

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!("▶ Task {} resumed", id),
        "status",
        None,
        None,
    );

    // Broadcast task-run-update
    let broadcaster = crate::event_system::EventBroadcaster::new(state.app_handle.clone());
    broadcaster.task_run_update(&id, "running", None, None);

    info!("Task {} unpaused", id);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Task run resumed"
    })))
}

/// Delete a task run.
pub async fn delete_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .delete_task_run(&id)
        .map(|deleted| {
            Json(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Task run deleted" } else { "Task run not found" }
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Generate an AI summary for a completed task run.
/// The summary includes:
/// - A paragraph summary of what was accomplished
/// - Whether the stated goal was achieved
/// - What remaining work exists (if goal not achieved)
pub async fn generate_task_summary(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("MCP API: Generating summary for task run: {}", id);

    // Run summary generation in a blocking task
    let db = state.app_state.checkpoint_db.clone();
    let task_id = id.clone();
    let doctor_handle = state.doctor_handle.clone();

    let result = tokio::task::spawn_blocking(move || {
        summary_generator::generate_task_summary(&db, &task_id, doctor_handle.as_ref(), None, None)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Task spawn error: {}", e),
        )
    })?;

    match result {
        Ok(summary_result) => Ok(Json(serde_json::json!({
            "success": true,
            "summary": summary_result.summary,
            "goal_achieved": summary_result.goal_achieved,
            "remaining_work": summary_result.remaining_work,
        }))),
        Err(e) => {
            warn!("Failed to generate summary for task {}: {}", id, e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, e))
        }
    }
}

/// Get the auto-continue setting for a specific task run.
pub async fn get_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_auto_continue(&id)
        .map(|auto_continue| {
            Json(serde_json::json!({
                "id": id,
                "auto_continue": auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for setting auto-continue on a task run.
#[derive(Debug, Deserialize)]
pub struct SetTaskAutoContinueRequest {
    auto_continue: bool,
}

/// Set the auto-continue setting for a specific task run.
pub async fn set_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<SetTaskAutoContinueRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .set_task_auto_continue(&id, req.auto_continue)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "id": id,
                "auto_continue": req.auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for resuming a task run.
#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    /// The user's message text
    message: String,
}

/// Send a user message to an active AI session via HTTP.
///
/// This is the HTTP equivalent of the `send_user_message` Tauri command.
/// It emits an ai-output event, persists to output_log, handles first
/// interaction detection, and forwards the message to the Claude session.
pub async fn send_message_to_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!(
        "HTTP send_message: task_run_id={}, message_len={}",
        id,
        req.message.len()
    );

    // Verify task run exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get SessionManager via Tauri managed state (same pattern as resume_task_run)
    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    // Find the active session for this task run
    let session = match session_manager.get(&id) {
        Some(s) => s,
        None => {
            return Ok(Json(serde_json::json!({
                "success": false,
                "error": format!("No active session found for task_run_id: {}", id),
                "state": "not_found"
            })));
        }
    };

    // Build session context so the output event is associated with this task run
    let session_ctx = AiSessionContext::setup(&id, "Ad-hoc Chat");
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_ai_output(
            &state.app_handle,
            &req.message,
            "user_message",
            None,
            Some(&session_ctx),
        );
    }));

    // Persist user message to output_log for recap/summary generation
    if let Err(e) = state.app_state.checkpoint_db.append_task_output_ex(
        &id,
        &format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", req.message),
        false,
        false,
    ) {
        warn!("Failed to persist user message to output_log: {}", e);
    }

    // Build the effective message by prepending any pending context (system notes
    // from workflow generation, etc.) and the first-interaction note if applicable.
    let mut prefix_parts: Vec<String> = Vec::new();

    // Drain pending context (system notes queued since last user message)
    if let Some(pending) = session_manager.drain_pending_context(&id) {
        info!(
            "Prepending pending context to user message for task_run_id={}",
            id
        );
        prefix_parts.push(pending);
    }

    // First-interaction context switch
    if !session.has_user_interacted() {
        info!(
            "First user interaction detected for task_run_id={}, injecting context switch note",
            id
        );
        prefix_parts.push(
            "[SYSTEM NOTE: A user is now watching and interacting with this session. \
             Please acknowledge their message and respond conversationally while continuing \
             your work. The user's message follows.]"
                .to_string(),
        );
    }

    let effective_message = if prefix_parts.is_empty() {
        req.message.clone()
    } else {
        prefix_parts.push(req.message.clone());
        prefix_parts.join("\n\n")
    };

    match session.send_user_message(&effective_message) {
        Ok(sent_immediately) => {
            let queued = !sent_immediately;
            let new_state = session.state();

            // Emit session state change event
            crate::commands::ai_session::emit_session_state(
                &state.app_handle,
                &id,
                session.session_id(),
                new_state,
            );

            info!(
                "HTTP send_message: task_run_id={}, queued={}, state={}",
                id,
                queued,
                new_state.as_event_str()
            );

            Ok(Json(serde_json::json!({
                "success": true,
                "queued": queued,
                "state": new_state.as_event_str()
            })))
        }
        Err(e) => {
            warn!("HTTP send_message failed for task_run_id={}: {}", id, e);
            Ok(Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to send message: {}", e),
                "state": session.state().as_event_str()
            })))
        }
    }
}

/// Get the current AI session state for a task run.
///
/// Returns the session state and interaction capabilities.
/// If no session exists for the task run, returns state "not_found".
pub async fn get_session_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task run exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get SessionManager via Tauri managed state
    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    match session_manager.get(&id) {
        Some(session) => {
            let current_state = session.state();
            Ok(Json(serde_json::json!({
                "state": current_state.as_event_str(),
                "can_send": current_state.can_send_message(),
                "can_interrupt": current_state.can_interrupt(),
                "session_id": session.session_id(),
                "user_interacted": session.has_user_interacted(),
                "pid": session.pid()
            })))
        }
        None => Ok(Json(serde_json::json!({
            "state": "not_found",
            "can_send": false,
            "can_interrupt": false
        }))),
    }
}

pub async fn sse_ai_output_for_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive};
    use futures_util::StreamExt as FuturesStreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let task_run_id = id.clone();
    info!(
        "SSE ai-output client connected for task_run_id={}",
        task_run_id
    );

    // Send initial catchup as first events
    let mut catchup_events: Vec<Result<Event, std::convert::Infallible>> = Vec::new();

    // 1. Current session state
    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());

    if let Some(sm) = &session_manager {
        if let Some(session) = sm.get(&task_run_id) {
            let state_data = serde_json::json!({
                "type": "session_state",
                "taskRunId": task_run_id,
                "state": session.state().as_event_str(),
                "canSend": session.state().can_send_message(),
                "canInterrupt": session.state().can_interrupt(),
                "userInteracted": session.has_user_interacted(),
            });
            if let Ok(json_str) = serde_json::to_string(&state_data) {
                catchup_events.push(Ok(Event::default()
                    .event("catchup/session_state")
                    .data(json_str)));
            }
        }
    }

    // 2. Recent output text
    let db = state.app_state.checkpoint_db.clone();
    let id_for_output = task_run_id.clone();
    if let Ok(Some(task_run)) = tokio::task::spawn_blocking(move || db.get_task_run(&id_for_output))
        .await
        .unwrap_or(Ok(None))
    {
        let output = &task_run.output_log;
        if !output.is_empty() {
            let tail = if output.len() > 5000 {
                let mut start = output.len() - 5000;
                // Find the nearest char boundary to avoid panic on multi-byte UTF-8
                while start < output.len() && !output.is_char_boundary(start) {
                    start += 1;
                }
                &output[start..]
            } else {
                output.as_str()
            };
            let output_data = serde_json::json!({
                "type": "output_catchup",
                "taskRunId": task_run_id,
                "text": tail,
            });
            if let Ok(json_str) = serde_json::to_string(&output_data) {
                catchup_events.push(Ok(Event::default().event("catchup/output").data(json_str)));
            }
        }
    }

    // Subscribe to broadcast channel
    let event_rx = state.app_state.event_broadcast.subscribe();
    let filter_id = task_run_id.clone();

    // Filter broadcast events to only this task run's ai-output and session-state
    let live_stream = FuturesStreamExt::filter_map(BroadcastStream::new(event_rx), move |result| {
        let filter_id = filter_id.clone();
        async move {
            match result {
                Ok(event) => {
                    let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                    if channel != "ai-output" && channel != "session-state" {
                        return None;
                    }

                    // Check taskRunId in payload matches
                    let payload = event.get("payload")?;
                    let event_task_run_id = payload
                        .get("taskRunId")
                        .or_else(|| payload.get("task_run_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if event_task_run_id != filter_id {
                        return None;
                    }

                    match serde_json::to_string(payload) {
                        Ok(json_str) => {
                            let event_name = format!("chat/{}", channel);
                            Some(Ok(Event::default().event(event_name).data(json_str)))
                        }
                        Err(_) => None,
                    }
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    Some(Ok(Event::default().event("chat/warning").data(format!(
                        "{{\"message\":\"Skipped {} events due to lag\"}}",
                        n
                    ))))
                }
            }
        }
    });

    // Combine catchup events with live stream
    let catchup_stream = futures_util::stream::iter(catchup_events);
    let combined = catchup_stream.chain(live_stream);

    Sse::new(combined).keep_alive(KeepAlive::default())
}

/// Request body for creating an ad-hoc AI session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    /// Name for the AI session
    #[serde(default = "default_session_name")]
    task_name: String,
}

fn default_session_name() -> String {
    "Ad-hoc Chat".to_string()
}

/// Create an ad-hoc AI session not tied to a workflow.
///
/// Creates a task_run record and spawns a new AI session.
pub async fn create_ai_session(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("Creating ad-hoc AI session: {}", req.task_name);

    let task_run_id = uuid::Uuid::new_v4().to_string();

    // Create task run record using builder pattern
    let db = state.app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let name_clone = req.task_name.clone();
    tokio::task::spawn_blocking(move || {
        let input = CreateTaskRunInput::new(id_clone, name_clone)
            .with_prompt("Ad-hoc AI session")
            .with_workflow_type("chat");
        db.create_task_run(&input)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Get SessionManager and spawn a new session
    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    // Determine working directory (use parent of runner project)
    let working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let system_prompt = "You are an AI assistant in a session initiated from the qontinui \
        mobile app. Respond helpfully and conversationally. The user may ask about anything — \
        workflows, automation, code questions, or general topics."
        .to_string();

    // Create session context so output is properly captured and associated
    let session_ctx = AiSessionContext::setup(&task_run_id, &req.task_name);

    // Spawn the Claude session
    match crate::claude_session::ClaudeSession::spawn(
        &working_dir,
        &task_run_id,
        &state.app_handle,
        Some(session_ctx),
        None, // finding_ctx
        None, // progress_ctx
        None, // pid_tracker
        None, // model_override
    ) {
        Ok(session) => {
            let session = Arc::new(session);

            // Register with session manager
            if let Err(e) = session_manager.register(&task_run_id, session.clone()) {
                warn!("Failed to register AI session: {}", e);
                return Ok(Json(serde_json::json!({
                    "id": task_run_id,
                    "task_name": req.task_name,
                    "state": "error",
                    "error": format!("Session registration failed: {}", e)
                })));
            }

            // Emit initial ready state
            crate::commands::ai_session::emit_session_state(
                &state.app_handle,
                &task_run_id,
                &task_run_id,
                session.state(),
            );

            // Send the system prompt as initial prompt
            if let Err(e) = session.send_initial_prompt(&system_prompt) {
                warn!("Failed to send initial prompt for AI session: {}", e);
                return Ok(Json(serde_json::json!({
                    "id": task_run_id,
                    "task_name": req.task_name,
                    "state": "error",
                    "error": format!("Failed to send initial prompt: {}", e)
                })));
            }

            // Emit processing state
            crate::commands::ai_session::emit_session_state(
                &state.app_handle,
                &task_run_id,
                &task_run_id,
                session.state(),
            );

            info!("AI session created: task_run_id={}", task_run_id);
            Ok(Json(serde_json::json!({
                "id": task_run_id,
                "task_name": req.task_name,
                "state": "ready"
            })))
        }
        Err(e) => {
            warn!("Failed to create AI session: {}", e);
            Ok(Json(serde_json::json!({
                "id": task_run_id,
                "task_name": req.task_name,
                "state": "error",
                "error": format!("Session creation failed: {}", e)
            })))
        }
    }
}

/// Generate a workflow from an AI session's conversation context
pub async fn generate_workflow_from_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Get output log from DB
    let db = state.app_state.checkpoint_db.clone();
    let id_clone = id.clone();
    let output_log = tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Task failed: {}", e),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
        })?
        .unwrap_or_default();

    if output_log.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No conversation history available".to_string(),
        ));
    }

    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("Generate workflow from chat conversation")
        .to_string();
    let include_ui_bridge = req
        .get("include_ui_bridge_instructions")
        .and_then(|v| v.as_bool());

    let request = crate::workflow_generation::GenerateWorkflowRequest {
        description,
        inline_context: Some(format!(
            "The following is a conversation between a user and an AI assistant. \
             Use this conversation context to generate an appropriate workflow:\n\n{}",
            output_log
        )),
        category: None,
        tags: None,
        max_iterations: None,
        provider: None,
        model: None,
        skip_ai_summary: None,
        log_source_selection: None,
        prompt_template: None,
        auto_include_contexts: Some(true),
        context_ids: None,
        max_fix_iterations: Some(3),
        discovery_mode: None,
        include_ui_bridge_instructions: include_ui_bridge,
        reflection_mode: Some(true),
        investigate_codebase: Some(true),
        include_design_guidance: None,
        auto_run: None,
        model_overrides: None,
        generate_specification: Some(true),
        verification_depth: None,
        discover_ui_bridge_specs: None,
        simple_mode: None,
    };

    let doctor_handle = state.doctor_handle.clone();
    let db2 = state.app_state.checkpoint_db.clone();
    let artifact_task_run_id = id.clone();

    let gen_result = tokio::task::spawn_blocking(move || {
        let gen_result = db2.with_conn(|conn| {
            let (response, mut artifact) = crate::workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(conn),
                None,
            );
            artifact.task_run_id = Some(artifact_task_run_id.clone());
            if let Err(e) = db2.save_pipeline_artifact(&artifact) {
                tracing::warn!("Failed to save pipeline artifact: {}", e);
            }
            Ok(response)
        });
        match gen_result {
            Ok(response) => response,
            Err(e) => crate::workflow_generation::GenerateWorkflowResponse {
                workflow: None,
                validation_errors: vec![],
                success: false,
                error: Some(format!("Database error during generation: {}", e)),
                model_used: None,
                verification_iterations: vec![],
                hardening_summary: None,
                discovery_calls: vec![],
                acceptance_criteria: None,
                quality_report: None,
                confidence_score: None,
            },
        }
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Generation task failed: {}", e),
        )
    })?;

    // Store generated_workflow_id in result_data (mirrors Tauri IPC command behavior)
    if gen_result.success {
        if let Some(ref workflow) = gen_result.workflow {
            let result_data = serde_json::json!({
                "generated_workflow_id": &workflow.id,
                "generated_workflow_name": &workflow.name,
            });
            let db3 = state.app_state.checkpoint_db.clone();
            let trid = id.clone();
            let rd_str = result_data.to_string();
            if let Err(e) =
                tokio::task::spawn_blocking(move || db3.update_task_run_result_data(&trid, &rd_str))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            {
                tracing::warn!("Failed to update chat task run result_data: {}", e);
            }
        }
    }

    Ok(Json(serde_json::json!({
        "success": gen_result.success,
        "workflow": gen_result.workflow,
        "error": gen_result.error,
        "validation_errors": gen_result.validation_errors,
        "model_used": gen_result.model_used
    })))
}

/// Rename a task run
pub async fn rename_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let new_name = req
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing 'name' field".to_string()))?
        .to_string();

    let db = state.app_state.checkpoint_db.clone();
    let id_clone = id.clone();
    let name_clone = new_name.clone();
    tokio::task::spawn_blocking(move || db.update_task_run_name(&id_clone, &name_clone))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Task failed: {}", e),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "id": id,
        "task_name": new_name
    })))
}

// ============================================================================
// Approval Gate Endpoints
// ============================================================================

/// List pending approvals for a task run.
pub async fn list_approvals(
    axum::extract::Path(task_run_id): axum::extract::Path<String>,
) -> Result<
    Json<Vec<crate::unified_workflow_executor::approval::ApprovalRequest>>,
    (StatusCode, String),
> {
    let registry = crate::unified_workflow_executor::approval::get_approval_registry();
    let pending = registry.get_pending_for_execution(&task_run_id).await;
    Ok(Json(pending))
}

/// Get a specific approval request.
pub async fn get_approval(
    axum::extract::Path((task_run_id, approval_id)): axum::extract::Path<(String, String)>,
) -> Result<Json<crate::unified_workflow_executor::approval::ApprovalRequest>, (StatusCode, String)>
{
    let registry = crate::unified_workflow_executor::approval::get_approval_registry();
    match registry.get_pending(&approval_id).await {
        Some(request) if request.execution_id == task_run_id => Ok(Json(request)),
        Some(_) => Err((
            StatusCode::NOT_FOUND,
            format!(
                "Approval '{}' not found for task run '{}'",
                approval_id, task_run_id
            ),
        )),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("No pending approval found with ID '{}'", approval_id),
        )),
    }
}

/// Request body for responding to an approval.
#[derive(Debug, Deserialize)]
pub struct ApprovalResponseBody {
    pub action: String, // "approve", "reject", "abort"
    pub comment: Option<String>,
}

/// Respond to a pending approval request.
pub async fn respond_to_approval(
    axum::extract::Path((_task_run_id, approval_id)): axum::extract::Path<(String, String)>,
    Json(body): Json<ApprovalResponseBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let approved = body.action == "approve";
    let response = crate::unified_workflow_executor::approval::ApprovalResponse {
        approved,
        action: body.action.clone(),
        comment: body.comment,
    };

    let registry = crate::unified_workflow_executor::approval::get_approval_registry();
    registry
        .resolve(&approval_id, response)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    Ok(Json(serde_json::json!({
        "status": "resolved",
        "approval_id": approval_id,
        "action": body.action,
        "approved": approved,
    })))
}

/// Get approval gate history (resolved approvals) for a task run.
pub async fn get_approval_gates(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(task_run_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();
    let id = task_run_id.clone();
    let gates = tokio::task::spawn_blocking(move || db.get_approval_gates_for_task_run(&id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Task failed: {}", e),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
        })?;

    Ok(Json(serde_json::json!(gates)))
}

// ============================================================================
// End Task Run HTTP API Handlers
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/task-runs", get(list_task_runs).post(create_task_run))
        .route("/task-runs/running", get(list_running_task_runs))
        .route("/task-runs/session", post(create_ai_session))
        .route("/task-runs/{id}", get(get_task_run).delete(delete_task_run))
        .route("/task-runs/{id}/output", get(get_task_output))
        .route("/task-runs/{id}/workflow-state", get(get_workflow_state))
        .route("/task-runs/{id}/result-data", get(get_task_run_result_data))
        .route(
            "/task-runs/{id}/orchestrator-state",
            get(get_workflow_state),
        ) // Alias for backward compatibility
        .route("/task-runs/{id}/full-state", get(get_full_workflow_state)) // Full state for restart recovery
        .route("/task-runs/{id}/stop", post(stop_task_run))
        .route("/task-runs/{id}/pause", post(pause_task_run))
        .route("/task-runs/{id}/unpause", post(unpause_task_run))
        .route(
            "/task-runs/{id}/auto-continue",
            get(get_task_auto_continue).put(set_task_auto_continue),
        )
        .route("/task-runs/{id}/resume", post(resume_task_run))
        .route(
            "/task-runs/{id}/generate-summary",
            post(generate_task_summary),
        )
        .route("/task-runs/{id}/events", get(get_task_run_events))
        .route("/task-runs/{id}/screenshots", get(get_task_run_screenshots))
        .route(
            "/task-runs/{id}/playwright-results",
            get(get_task_run_playwright_results),
        )
        .route("/execution-spans", get(get_execution_spans))
        .route("/task-runs/{id}/migrate-logs", post(migrate_task_run_logs))
        .route("/task-runs/{id}/checkpoints", get(get_task_run_checkpoints))
        .route(
            "/task-runs/{id}/verification-results",
            get(get_task_run_verification_results),
        )
        .route(
            "/task-runs/{id}/verification-phase-results",
            get(get_task_run_verification_phase_results),
        )
        .route("/task-runs/{id}/mcp-calls", get(get_task_run_mcp_calls))
        .route(
            "/task-runs/{id}/api-requests",
            get(get_task_run_api_requests),
        )
        .route("/task-runs/{id}/awas-steps", get(get_task_run_awas_steps))
        .route("/task-runs/{id}/knowledge", get(get_task_run_knowledge))
        .route(
            "/task-runs/{id}/steps/{checkpoint_id}/progress",
            get(get_step_progress_markers),
        )
        .route("/task-runs/{id}/message", post(send_message_to_session))
        .route("/task-runs/{id}/session-state", get(get_session_state))
        .route(
            "/task-runs/{id}/generate-workflow",
            post(generate_workflow_from_session),
        )
        .route("/task-runs/{id}/rename", post(rename_task_run))
        .route(
            "/task-runs/{id}/stream/ai-output",
            get(sse_ai_output_for_task_run),
        )
        .route("/task-runs/{id}/approvals", get(list_approvals))
        .route("/task-runs/{id}/approvals/{aid}", get(get_approval))
        .route(
            "/task-runs/{id}/approvals/{aid}/respond",
            post(respond_to_approval),
        )
        .route("/task-runs/{id}/approval-gates", get(get_approval_gates))
        .route("/current-execution/steps", get(get_current_execution_steps))
        .route("/current-execution/batch", get(get_current_execution_batch))
        .route("/task-runs/{id}/usage", get(get_task_run_usage))
        .route("/traces/{trace_id}", get(get_trace_correlation))
}
