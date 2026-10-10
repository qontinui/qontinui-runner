//! Read-only data query endpoints for task runs.
//!
//! This module contains all pure read-only data query handler functions
//! extracted from `task_runs.rs`. These follow the pattern: parse query
//! params, get DB connection, query, format response.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::error;

use crate::mcp::types::ApiState;

// =============================================================================
// Query / Path parameter structs
// =============================================================================

/// Step execution data for dashboard widget.
#[derive(Debug, Serialize)]
pub struct StepExecutionData {
    id: String,
    step_type: String,
    step_name: String,
    status: String, // "pending", "running", "success", "failed"
    /// Workflow phase: "setup", "verification", "agentic", or "completion"
    phase: Option<String>,
    /// Step index within the phase
    step_index: Option<i64>,
    /// Stage index for multi-stage workflows (0-indexed)
    stage_index: Option<u32>,
    /// Iteration number for verification/agentic phases (1-indexed)
    iteration: Option<i64>,
    start_time: Option<i64>,
    end_time: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    output: Option<String>,
    // Shell command specific fields
    command: Option<String>,
    working_directory: Option<String>,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
    /// Original command template (with {{variable}} placeholders) - only present if variables were used
    template_command: Option<String>,
    /// Variables that were resolved during command execution (name -> resolved value)
    resolved_variables: Option<serde_json::Value>,
}

/// Result of aggregating step events into execution data.
#[derive(Debug, Serialize)]
pub struct AggregatedStepData {
    pub steps: Vec<StepExecutionData>,
    pub has_setup: bool,
    pub has_verification: bool,
    pub has_agentic: bool,
}

// =============================================================================
// Handler functions
// =============================================================================

/// Aggregate raw task run events into per-step execution summaries.
///
/// This extracts the event aggregation logic that was previously inline in
/// `get_current_execution_steps`, making it testable and reusable.
/// Events with the same action_id (or synthesized key from step_name + step_index + iteration)
/// are merged so that start and complete events produce a single `StepExecutionData`.
pub fn aggregate_step_events(events: &[crate::database::TaskRunEvent]) -> AggregatedStepData {
    use std::collections::HashMap;

    let mut step_map: HashMap<String, StepExecutionData> = HashMap::new();

    for event in events {
        let event_type = event.event_type.as_str();
        if event_type != "step_execution"
            && event_type != "command"
            && event_type != "shell_command"
        {
            continue;
        }

        let data: Option<serde_json::Value> = event
            .data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let event_subtype = event.event_subtype.as_deref().unwrap_or("");
        let message = event.message.as_str();

        let step_name = data
            .as_ref()
            .and_then(|d| d.get("step_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| message.to_string());

        let step_index = data
            .as_ref()
            .and_then(|d| d.get("step_index"))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);

        let step_type_str = data
            .as_ref()
            .and_then(|d| d.get("step_type"))
            .and_then(|v| v.as_str())
            .unwrap_or(event_type)
            .to_string();

        let iteration_for_key = data
            .as_ref()
            .and_then(|d| d.get("iteration"))
            .and_then(|v| v.as_i64());

        let key = event.action_id.clone().unwrap_or_else(|| {
            if let Some(iter) = iteration_for_key {
                format!("{}:{}:{}", step_name, step_index, iter)
            } else {
                format!("{}:{}", step_name, step_index)
            }
        });

        let event_timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            .ok()
            .map(|dt| dt.timestamp_millis());

        let status = match event_subtype {
            "start" => "running",
            "complete" | "success" => "success",
            "error" | "failed" => "failed",
            _ => "pending",
        }
        .to_string();

        if let Some(existing) = step_map.get_mut(&key) {
            let should_update_status = match (existing.status.as_str(), status.as_str()) {
                ("failed", _) => false,
                ("running", "success") | ("running", "failed") => true,
                ("pending", "success") | ("pending", "failed") => true,
                ("success", "failed") => true,
                ("success", "success") | ("success", "running") => false,
                _ => status != "running",
            };
            if should_update_status {
                existing.status = status;
            }

            if let Some(d) = &data {
                if existing.phase.is_none() {
                    if let Some(v) = d.get("phase").and_then(|v| v.as_str()) {
                        existing.phase = Some(v.to_string());
                    }
                }
                if existing.iteration.is_none() {
                    if let Some(v) = d.get("iteration").and_then(|v| v.as_i64()) {
                        existing.iteration = Some(v);
                    }
                }
                // Update stage_index if not already set
                if existing.stage_index.is_none() {
                    if let Some(v) = d.get("stage_index").and_then(|v| v.as_u64()) {
                        existing.stage_index = Some(v as u32);
                    }
                }
                if let Some(v) = d.get("duration_ms").and_then(|v| v.as_i64()) {
                    existing.duration_ms = Some(v);
                } else if existing.duration_ms.is_none() {
                    existing.duration_ms = event.duration_ms;
                }
                if let Some(v) = d.get("end_time").and_then(|v| v.as_i64()) {
                    existing.end_time = Some(v);
                }
                if let Some(v) = d.get("exit_code").and_then(|v| v.as_i64()) {
                    existing.exit_code = Some(v as i32);
                }
                if let Some(v) = d.get("stdout").and_then(|v| v.as_str()) {
                    existing.stdout = Some(v.to_string());
                }
                if let Some(v) = d.get("stderr").and_then(|v| v.as_str()) {
                    existing.stderr = Some(v.to_string());
                }
                if let Some(v) = d.get("error").and_then(|v| v.as_str()) {
                    existing.error = Some(v.to_string());
                }
                if let Some(v) = d.get("output").and_then(|v| v.as_str()) {
                    existing.output = Some(v.to_string());
                }
            }
        } else {
            let phase = data
                .as_ref()
                .and_then(|d| d.get("phase"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let iteration = data
                .as_ref()
                .and_then(|d| d.get("iteration"))
                .and_then(|v| v.as_i64());

            // Extract stage_index from event data (for multi-stage workflows)
            let stage_index = data
                .as_ref()
                .and_then(|d| d.get("stage_index"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);

            let step_data = StepExecutionData {
                id: event.id.to_string(),
                step_type: step_type_str,
                step_name,
                status,
                phase,
                step_index: if step_index >= 0 {
                    Some(step_index)
                } else {
                    None
                },
                stage_index,
                iteration,
                start_time: event_timestamp,
                end_time: data
                    .as_ref()
                    .and_then(|d| d.get("end_time"))
                    .and_then(|v| v.as_i64()),
                duration_ms: data
                    .as_ref()
                    .and_then(|d| d.get("duration_ms"))
                    .and_then(|v| v.as_i64())
                    .or(event.duration_ms),
                error: data
                    .as_ref()
                    .and_then(|d| d.get("error"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                output: data
                    .as_ref()
                    .and_then(|d| d.get("output"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                command: data
                    .as_ref()
                    .and_then(|d| d.get("command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                working_directory: data
                    .as_ref()
                    .and_then(|d| d.get("working_directory"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                exit_code: data
                    .as_ref()
                    .and_then(|d| d.get("exit_code"))
                    .and_then(|v| v.as_i64())
                    .map(|i| i as i32),
                stdout: data
                    .as_ref()
                    .and_then(|d| d.get("stdout"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                stderr: data
                    .as_ref()
                    .and_then(|d| d.get("stderr"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                template_command: data
                    .as_ref()
                    .and_then(|d| d.get("template_command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                resolved_variables: data
                    .as_ref()
                    .and_then(|d| d.get("resolved_variables"))
                    .cloned(),
            };

            step_map.insert(key, step_data);
        }
    }

    let has_setup = step_map
        .values()
        .any(|s| s.phase.as_deref() == Some("setup"));
    let has_verification = step_map
        .values()
        .any(|s| s.phase.as_deref() == Some("verification"));
    let has_agentic = step_map
        .values()
        .any(|s| s.phase.as_deref() == Some("agentic"));

    let mut steps: Vec<StepExecutionData> = step_map.into_values().collect();
    steps.sort_by_key(|s| s.start_time);

    AggregatedStepData {
        steps,
        has_setup,
        has_verification,
        has_agentic,
    }
}

/// Batch endpoint: returns the running task, its aggregated step data, and
/// completed verification iterations in a single response.
/// This replaces multiple round-trips the frontend would otherwise need.
pub async fn get_current_execution_batch(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // get_running_task_step_data / get_completed_verification_iterations not yet on PgDb
    // Fall back to composing from existing PgDb methods
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let running_tasks = state
        .app_state
        .pg_db
        .get_running_task_runs(Some(port))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if running_tasks.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": true,
            "task_run_id": null,
            "executions": [],
            "completed_iterations": [],
            "count": 0,
            "message": "No running task"
        })));
    }

    let task = &running_tasks[0];
    let events = state
        .app_state
        .pg_db
        .get_task_run_events(&task.id, None, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let aggregated = aggregate_step_events(&events);

    let completed_iterations = state
        .app_state
        .pg_db
        .get_all_verification_phase_results(&task.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.get("iteration").and_then(|i| i.as_i64()))
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "success": true,
        "task_run_id": task.id,
        "workflow_name": task.workflow_name,
        "workflow_type": task.workflow_type,
        "workflow_start_time": task.created_at,
        "has_setup": aggregated.has_setup,
        "has_verification": aggregated.has_verification,
        "has_agentic": aggregated.has_agentic,
        "executions": aggregated.steps,
        "completed_iterations": completed_iterations,
        "count": aggregated.steps.len()
    })))
}

/// Get per-phase token usage breakdown for a task run.
pub async fn get_task_run_usage(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let usage = state
        .app_state
        .pg_db
        .get_phase_token_usage(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Compute totals
    let total_input: u64 = usage.iter().map(|u| u.input_tokens).sum();
    let total_output: u64 = usage.iter().map(|u| u.output_tokens).sum();
    let total_cost: u64 = usage.iter().map(|u| u.cost_cents).sum();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "phases": usage,
        "totals": {
            "input_tokens": total_input,
            "output_tokens": total_output,
            "cost_cents": total_cost,
        }
    })))
}

/// Get cross-service trace correlation data for a given trace ID.
///
/// Queries both execution_spans and error_events tables to return
/// all data associated with a trace, enabling cross-service debugging.
pub async fn get_trace_correlation(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(trace_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Trace correlation uses execution_spans — use PG get_execution_spans
    let spans = state
        .app_state
        .pg_db
        .get_execution_spans(&trace_id)
        .await
        .unwrap_or_default();

    // error_events not yet queryable by trace_id on PgDb — return empty
    let errors: Vec<serde_json::Value> = Vec::new();

    Ok(Json(serde_json::json!({
        "trace_id": trace_id,
        "execution_spans": spans,
        "error_events": errors,
        "span_count": spans.len(),
        "error_count": errors.len(),
    })))
}

/// Get blame attributions for a task run.
///
/// Returns all blame reports from each iteration, plus aggregate statistics.
/// Data comes from iteration_results stored in the task run's result_data.
///
/// `GET /task-runs/{id}/blame`
pub async fn get_task_run_blame(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Get the task run
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Parse result_data to extract iteration_results with blame_json
    let mut blame_reports: Vec<serde_json::Value> = Vec::new();
    let mut total_attributions = 0u32;
    let mut total_oscillating = 0u32;
    let mut total_reverts = 0u32;

    if let Some(ref result_data) = task.result_data {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(result_data) {
            if let Some(iterations) = data.get("iteration_results").and_then(|v| v.as_array()) {
                for iter_result in iterations {
                    if let Some(blame_json) = iter_result.get("blame_json").and_then(|v| v.as_str())
                    {
                        if let Ok(report) = serde_json::from_str::<serde_json::Value>(blame_json) {
                            let iteration = iter_result
                                .get("iteration")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);

                            let attr_count = report
                                .get("attributions")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            let osc_count = report
                                .get("oscillating_files")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            let rev_count = report
                                .get("revert_patterns")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);

                            total_attributions += attr_count as u32;
                            total_oscillating += osc_count as u32;
                            total_reverts += rev_count as u32;

                            blame_reports.push(serde_json::json!({
                                "iteration": iteration,
                                "report": report,
                            }));
                        }
                    }
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "total_iterations_with_blame": blame_reports.len(),
        "total_attributions": total_attributions,
        "total_oscillating_files": total_oscillating,
        "total_revert_patterns": total_reverts,
        "blame_reports": blame_reports,
    })))
}
