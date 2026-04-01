//! Task run inspection endpoints.
//!
//! Read-only GET endpoints for inspecting task run state:
//! - `/task-run/{id}/events` — task run events with optional filtering
//! - `/task-run/{id}/output` — output log (plain or SSE streaming)
//! - `/task-run/{id}/state` — current execution state, checkpoints, progress

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::ApiState;

// =============================================================================
// Query parameter structs
// =============================================================================

/// Query parameters for the events endpoint.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Maximum number of events to return (default: 50).
    limit: Option<u32>,
    /// Filter by event type (optional).
    event_type: Option<String>,
}

/// Query parameters for the output endpoint.
#[derive(Debug, Deserialize)]
pub struct OutputQuery {
    /// If true, return Server-Sent Events that poll output every 2s until task completes.
    #[serde(default)]
    stream: bool,
    /// Byte offset for incremental fetching (non-streaming mode).
    offset: Option<usize>,
}

// =============================================================================
// GET /task-run/{id}/events
// =============================================================================

/// Returns task run events for a specific task run.
///
/// Supports optional `limit` (default 50) and `event_type` filter query params.
pub async fn get_events(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let limit = query.limit.or(Some(50));
    let events = state
        .app_state
        .pg_db
        .get_task_run_events(&id, query.event_type.as_deref(), limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "data": events
    })))
}

// =============================================================================
// GET /task-run/{id}/output
// =============================================================================

/// Returns the output log for a specific task run.
///
/// When `stream=false` (default): returns `{ "data": { "output": "...", "length": N } }`.
/// When `stream=true`: returns SSE that polls output every 2s until the task completes.
/// The `offset` param (non-streaming) slices the output from that byte position.
pub async fn get_output(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<OutputQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    use axum::response::IntoResponse;

    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    if query.stream {
        // SSE streaming mode
        let task_id = id.clone();
        let pg_db = state.app_state.pg_db.clone();
        let mut last_len: usize = 0;

        let stream = async_stream::stream! {
            loop {
                // Fetch current output
                let output = match pg_db.get_task_output(&task_id).await {
                    Ok(o) => o,
                    Err(e) => {
                        let err_event = Event::default()
                            .event("error")
                            .data(format!("{{\"error\": \"{}\"}}", e));
                        yield Ok::<_, std::convert::Infallible>(err_event);
                        break;
                    }
                };

                let current_len = output.len();
                if current_len > last_len {
                    // Send new chunk
                    let new_data = &output[last_len..];
                    let chunk_event = Event::default()
                        .event("output")
                        .data(serde_json::json!({
                            "chunk": new_data,
                            "offset": last_len,
                            "length": current_len,
                        }).to_string());
                    yield Ok(chunk_event);
                    last_len = current_len;
                }

                // Check if task is complete
                let is_done = match pg_db.get_task_run(&task_id).await {
                    Ok(Some(tr)) => {
                        let s = tr.status.as_str();
                        s == "completed" || s == "failed" || s == "stopped" || s == "cancelled"
                    }
                    _ => true, // If we can't read, stop
                };

                if is_done {
                    // Send final event
                    let done_event = Event::default()
                        .event("done")
                        .data(serde_json::json!({
                            "length": last_len,
                        }).to_string());
                    yield Ok(done_event);
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        };

        Ok(Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response())
    } else {
        // Non-streaming mode
        let output = state
            .app_state
            .pg_db
            .get_task_output(&id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let (sliced, length) = if let Some(offset) = query.offset {
            if offset < output.len() {
                (output[offset..].to_string(), output.len())
            } else {
                (String::new(), output.len())
            }
        } else {
            let len = output.len();
            (output, len)
        };

        Ok(Json(serde_json::json!({
            "data": {
                "output": sliced,
                "length": length,
            }
        }))
        .into_response())
    }
}

// =============================================================================
// GET /task-run/{id}/state
// =============================================================================

/// Returns the current execution state of a task run.
///
/// Combines data from:
/// - `workflow_execution_state` table (current state/phase/iteration)
/// - `workflow_step_checkpoints` table (checkpoint history)
/// - `step_progress_markers` table (progress markers via current step)
pub async fn get_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let db = &state.app_state.pg_db;

    // 1. Get workflow execution state
    let exec_state = db
        .get_workflow_execution_state(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // 2. Get all checkpoints for this execution
    let checkpoints = db
        .get_all_workflow_step_checkpoints(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // 3. Get current step progress
    let step_progress = db
        .get_current_step_progress(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let response = match exec_state {
        Some(es) => serde_json::json!({
            "data": {
                "execution_id": es.execution_id,
                "state_name": es.state_name,
                "phase": es.phase,
                "iteration": es.iteration,
                "updated_at": es.updated_at,
                "checkpoints": checkpoints,
                "step_progress": step_progress.and_then(|sp| serde_json::to_value(&sp).ok()),
            }
        }),
        None => serde_json::json!({
            "data": {
                "execution_id": id,
                "state_name": null,
                "phase": null,
                "iteration": null,
                "updated_at": null,
                "checkpoints": checkpoints,
                "step_progress": step_progress.and_then(|sp| serde_json::to_value(&sp).ok()),
            }
        }),
    };

    Ok(Json(response))
}

// =============================================================================
// Route registration
// =============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/task-run/{id}/events", get(get_events))
        .route("/task-run/{id}/output", get(get_output))
        .route("/task-run/{id}/state", get(get_state))
}
