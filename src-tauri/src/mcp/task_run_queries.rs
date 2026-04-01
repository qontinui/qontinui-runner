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

/// Query parameters for task run events.
#[derive(Debug, Deserialize)]
pub struct TaskRunEventsQuery {
    event_type: Option<String>,
    limit: Option<u32>,
}

/// Query parameters for paginated checkpoints.
#[derive(Debug, Deserialize)]
pub struct CheckpointsQuery {
    /// Cursor for pagination (step_index to start from, exclusive).
    cursor: Option<i64>,
    /// Maximum number of checkpoints to return (default: 50).
    limit: Option<usize>,
}

/// Query parameters for verification results.
#[derive(Debug, Deserialize)]
pub struct VerificationResultsQuery {
    /// Filter by iteration number (optional)
    iteration: Option<u32>,
    /// Only show failed results (optional, default: false)
    #[serde(default)]
    failed_only: bool,
}

/// Query parameters for MCP calls endpoint.
#[derive(Debug, Deserialize)]
pub struct McpCallsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Query parameters for API requests endpoint.
#[derive(Debug, Deserialize)]
pub struct ApiRequestsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Query parameters for Playwright results endpoint.
#[derive(Debug, Deserialize)]
pub struct PlaywrightResultsQuery {
    /// Filter by status (optional: "passed", "failed", "skipped")
    status: Option<String>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Query parameters for knowledge endpoint.
#[derive(Debug, Deserialize)]
pub struct KnowledgeQuery {
    /// Filter by category (optional: "finding", "observation", "solution", etc.)
    category: Option<String>,
    /// Only show unresolved entries (optional, default: false)
    #[serde(default)]
    unresolved_only: bool,
}

/// Path parameters for step progress endpoint.
#[derive(Debug, Deserialize)]
pub struct StepProgressPath {
    id: String,
    checkpoint_id: String,
}

/// Query parameters for current execution steps.
#[derive(Debug, Deserialize)]
pub struct CurrentExecutionStepsQuery {
    /// Filter by step type (shell_command, prompt, verification, etc.)
    step_type: Option<String>,
    /// Maximum number of steps to return
    limit: Option<u32>,
}

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

/// Query parameters for execution spans.
#[derive(Debug, Deserialize)]
pub struct ExecutionSpansQuery {
    /// Filter by execution/task ID
    execution_id: Option<String>,
    /// Filter span names using SQL LIKE pattern (e.g., "workflow.%")
    name_pattern: Option<String>,
    /// Filter spans with duration >= this value
    min_duration_ms: Option<i64>,
    /// Maximum number of spans to return (default: 100)
    limit: Option<u32>,
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

/// Get events for a task run from SQLite (hybrid logging).
pub async fn get_task_run_events(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TaskRunEventsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let events = state.app_state.pg_db.get_task_run_events(&id, query.event_type.as_deref(), query.limit).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "events": events,
        "count": events.len()
    })))
}

/// Get step checkpoints for a task run with cursor-based pagination.
///
/// This endpoint supports efficient pagination for runs with 1000+ steps.
/// Use the `next_cursor` from the response to fetch the next page.
pub async fn get_task_run_checkpoints(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<CheckpointsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let limit = query.limit.unwrap_or(50).min(100); // Cap at 100 per page

    let (checkpoints, next_cursor) = state.app_state.pg_db.get_workflow_step_checkpoints_paginated(&id, query.cursor, limit).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "checkpoints": checkpoints,
        "count": checkpoints.len(),
        "cursor": query.cursor,
        "next_cursor": next_cursor,
        "has_more": next_cursor.is_some()
    })))
}

/// Get verification results for a task run.
///
/// Returns detailed verification test results from the orchestrator,
/// including issues, observations, suggestions, and raw output.
/// This is useful for AI agents to understand what specifically failed
/// during verification and why.
pub async fn get_task_run_verification_results(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<VerificationResultsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get verification results (PG returns Option<serde_json::Value>)
    let results_json = if let Some(iteration) = query.iteration {
        state
            .app_state
            .pg_db
            .get_iteration_verification_results(&id, iteration)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        state
            .app_state
            .pg_db
            .get_latest_verification_results(&id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    };

    // Extract results array from JSON (or empty)
    let results_array = results_json
        .and_then(|v| v.get("results").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    // Filter by failed_only if requested
    let results_array: Vec<_> = if query.failed_only {
        results_array.into_iter().filter(|r| {
            r.get("passed").and_then(|v| v.as_bool()) != Some(true)
        }).collect()
    } else {
        results_array
    };

    // Calculate summary stats
    let total = results_array.len();
    let passed = results_array.iter().filter(|r| r.get("passed").and_then(|v| v.as_bool()) == Some(true)).count();
    let failed = total - passed;
    let critical_failed = results_array.iter().filter(|r| {
        r.get("passed").and_then(|v| v.as_bool()) != Some(true)
            && r.get("is_critical").and_then(|v| v.as_bool()) == Some(true)
    }).count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "results": results_array,
        "summary": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "critical_failed": critical_failed,
            "all_passed": failed == 0
        },
        "query": {
            "iteration": query.iteration,
            "failed_only": query.failed_only
        }
    })))
}

/// Get workflow verification phase results for a task run.
///
/// Returns step-executor-based verification results from unified workflow execution,
/// grouped by iteration. Each iteration contains individual step results including
/// test results, check group results with individual check details, etc.
pub async fn get_task_run_verification_phase_results(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let _task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get all workflow verification phase results
    let results = state
        .app_state
        .pg_db
        .get_all_verification_phase_results(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let count = results.len();
    let passed_iterations = results
        .iter()
        .filter(|r| {
            r.get("all_passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    let failed_iterations = count - passed_iterations;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "results": results,
        "count": count,
        "passed_iterations": passed_iterations,
        "failed_iterations": failed_iterations
    })))
}

/// Get MCP tool calls for a task run.
///
/// Returns all MCP tool calls made during the task execution,
/// including server info, tool name, arguments, response, and timing.
/// Useful for AI to understand what external tools were used.
pub async fn get_task_run_mcp_calls(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<McpCallsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get MCP calls
    let result = state
        .app_state
        .pg_db
        .get_task_run_mcp_calls(&id, query.success)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let calls = if let Some(limit) = query.limit {
        result
            .calls
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>()
    } else {
        result.calls
    };

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "calls": calls,
        "summary": {
            "total": result.count,
            "success": result.success_count,
            "failed": result.failed_count
        },
        "query": {
            "success_filter": query.success,
            "limit": query.limit
        }
    })))
}

/// Get API requests for a task run.
///
/// Returns all API requests made during the task execution,
/// including method, URL, status, response, and timing.
pub async fn get_task_run_api_requests(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<ApiRequestsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get API requests
    let requests = state.app_state.pg_db.get_task_run_api_requests(&id).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let requests = if let Some(limit) = query.limit {
        requests
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>()
    } else {
        requests
    };

    // Calculate summary
    let total = requests.len();
    let success = requests.iter().filter(|r| r.success).count();
    let failed = total - success;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "requests": requests,
        "summary": {
            "total": total,
            "success": success,
            "failed": failed
        },
        "query": {
            "success_filter": query.success,
            "limit": query.limit
        }
    })))
}

/// Get Playwright test results for a task run.
///
/// Returns all Playwright test results including status, duration,
/// console output, page snapshots, and failure screenshots.
pub async fn get_task_run_playwright_results(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<PlaywrightResultsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get Playwright results
    let results = state.app_state.pg_db.get_task_run_playwright_results(&id).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let results = if let Some(limit) = query.limit {
        results.into_iter().take(limit as usize).collect::<Vec<_>>()
    } else {
        results
    };

    // Calculate summary
    let total = results.len();
    let passed = results.iter().filter(|r| r.status == "passed").count();
    let failed = results.iter().filter(|r| r.status == "failed").count();
    let skipped = results.iter().filter(|r| r.status == "skipped").count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "results": results,
        "summary": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "skipped": skipped
        },
        "query": {
            "status_filter": query.status,
            "limit": query.limit
        }
    })))
}

/// Get AWAS (Automated Web Agent System) steps for a task run.
///
/// Returns all AWAS operations including discovery, execution, and element extraction.
pub async fn get_task_run_awas_steps(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get AWAS steps
    let steps = state.app_state.pg_db.get_task_run_awas_steps(&id).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Calculate summary
    let total = steps.len();
    let success = steps.iter().filter(|s| s.success).count();
    let failed = total - success;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "steps": steps,
        "summary": {
            "total": total,
            "success": success,
            "failed": failed
        }
    })))
}

/// Get knowledge entries for a task run.
///
/// Returns accumulated knowledge from the task execution including:
/// - Findings (bugs, root causes identified)
/// - Observations (things noticed during execution)
/// - Solutions (fixes applied)
/// - Verification feedback (test failure context)
///
/// This helps the AI understand what was discovered and attempted.
pub async fn get_task_run_knowledge(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<KnowledgeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get knowledge entries
    let knowledge = state
        .app_state
        .pg_db
        .list_task_knowledge(&id, query.category.as_deref(), query.unresolved_only)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Calculate summary by category
    let total = knowledge.len();
    let findings = knowledge.iter().filter(|k| k.category == "finding").count();
    let observations = knowledge
        .iter()
        .filter(|k| k.category == "observation")
        .count();
    let solutions = knowledge
        .iter()
        .filter(|k| k.category == "solution")
        .count();
    let feedback = knowledge
        .iter()
        .filter(|k| k.category == "verification_feedback")
        .count();
    let unresolved = knowledge.iter().filter(|k| !k.is_resolved).count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "knowledge": knowledge,
        "summary": {
            "total": total,
            "by_category": {
                "finding": findings,
                "observation": observations,
                "solution": solutions,
                "verification_feedback": feedback
            },
            "unresolved": unresolved
        },
        "query": {
            "category_filter": query.category,
            "unresolved_only": query.unresolved_only
        }
    })))
}

/// Get progress markers for a specific step checkpoint.
///
/// Progress markers track intra-step progress (e.g., "analyzed 50/100 files").
pub async fn get_step_progress_markers(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(path): axum::extract::Path<StepProgressPath>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .pg_db
        .get_task_run(&path.id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Task run not found: {}", path.id),
            )
        })?;

    // Get progress markers for this checkpoint
    let markers = state
        .app_state
        .pg_db
        .get_step_progress_markers(&path.checkpoint_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Also get the latest marker for quick access
    let latest = state
        .app_state
        .pg_db
        .get_latest_step_progress_marker(&path.checkpoint_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "checkpoint_id": path.checkpoint_id,
        "markers": markers,
        "count": markers.len(),
        "latest": latest
    })))
}

/// Get step executions for the currently running task.
/// This endpoint combines running task detection with event querying,
/// so the frontend doesn't need to track task IDs.
///
/// Events are aggregated by step_name + step_index so that start and complete
/// events for the same step are merged into a single entry.
pub async fn get_current_execution_steps(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<CurrentExecutionStepsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use std::collections::HashMap;

    // Get running tasks
    let running_tasks = state
        .app_state
        .pg_db
        .get_running_task_runs(Some(
            state
                .app_state
                .api_port
                .load(std::sync::atomic::Ordering::Relaxed),
        ))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // If no running task, return empty
    if running_tasks.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": true,
            "task_run_id": null,
            "executions": [],
            "count": 0,
            "message": "No running task"
        })));
    }

    // Use the first running task (typically there's only one)
    let task = &running_tasks[0];

    // Get all events for this task (don't filter by event_type, we'll filter in code)
    let events = state.app_state.pg_db.get_task_run_events(&task.id, None, query.limit).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Aggregate events by action_id (preferred) or step_name + step_index to merge start/complete events
    // Using action_id is more reliable because it's generated from metadata and consistent across events
    // Key: String (action_id or synthesized from step_name + step_index)
    let mut step_map: HashMap<String, StepExecutionData> = HashMap::new();

    for event in events {
        // Only process step-related events
        let event_type = event.event_type.as_str();
        if event_type != "step_execution"
            && event_type != "command"
            && event_type != "shell_command"
        {
            continue;
        }
        // Parse the event data JSON to extract step information
        let data: Option<serde_json::Value> = event
            .data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let event_subtype = event.event_subtype.as_deref().unwrap_or("");
        let message = event.message.as_str();

        // Extract step identification
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

        // Filter by step type if specified
        if let Some(ref filter_type) = query.step_type {
            if !step_type_str
                .to_lowercase()
                .contains(&filter_type.to_lowercase())
            {
                continue;
            }
        }

        // Extract iteration early for use in fallback key
        let iteration_for_key = data
            .as_ref()
            .and_then(|d| d.get("iteration"))
            .and_then(|v| v.as_i64());

        // Use action_id as the primary key for aggregation (most reliable)
        // Fall back to synthesized key from step_name + step_index + iteration
        // Including iteration in fallback prevents merging steps from different iterations
        let key = event.action_id.clone().unwrap_or_else(|| {
            if let Some(iter) = iteration_for_key {
                format!("{}:{}:{}", step_name, step_index, iter)
            } else {
                format!("{}:{}", step_name, step_index)
            }
        });

        // Get the timestamp for this event
        let event_timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            .ok()
            .map(|dt| dt.timestamp_millis());

        // Determine status from event_subtype
        let status = match event_subtype {
            "start" => "running",
            "complete" | "success" => "success",
            "error" | "failed" => "failed",
            _ => "pending",
        }
        .to_string();

        // Check if we already have an entry for this step
        if let Some(existing) = step_map.get_mut(&key) {
            // Merge: prefer completion data over start data
            // Status priority: failed > success > running > pending
            // Once a step is marked as failed, it stays failed (even if we see a "complete" event)
            // This handles duplicate events where one might be error and one complete
            let should_update_status = match (existing.status.as_str(), status.as_str()) {
                // Never downgrade from failed (failed is highest priority)
                ("failed", _) => false,
                // Upgrade running/pending to anything terminal
                ("running", "success") | ("running", "failed") => true,
                ("pending", "success") | ("pending", "failed") => true,
                // Upgrade success only to failed (if somehow we get conflicting events)
                ("success", "failed") => true,
                // Don't change success to success (no-op) or downgrade success to running
                ("success", "success") | ("success", "running") => false,
                // Other cases: update
                _ => status != "running",
            };
            if should_update_status {
                existing.status = status;
            }

            // Update fields that are typically only in complete events
            if let Some(d) = &data {
                // Update phase if not already set
                if existing.phase.is_none() {
                    if let Some(v) = d.get("phase").and_then(|v| v.as_str()) {
                        existing.phase = Some(v.to_string());
                    }
                }
                // Update iteration if not already set
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
                // Try JSON data first, then fall back to event's top-level duration_ms
                if let Some(v) = d.get("duration_ms").and_then(|v| v.as_i64()) {
                    existing.duration_ms = Some(v);
                } else if existing.duration_ms.is_none() {
                    // Fall back to event's top-level duration_ms field
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
            // Extract phase from event data
            let phase = data
                .as_ref()
                .and_then(|d| d.get("phase"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Extract iteration from event data (for verification/agentic phases)
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

            // Create new entry
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
                // Try JSON data first, then fall back to event's top-level duration_ms
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

    // Get completed iterations from verification_phase_results
    // Steps from completed iterations should not show as "running"
    let completed_iterations: std::collections::HashSet<i64> = state
        .app_state
        .pg_db
        .get_all_verification_phase_results(&task.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.get("iteration").and_then(|i| i.as_i64()))
        .collect();

    // Find the maximum iteration number across all steps
    // This helps detect stale "running" steps in older iterations
    let max_iteration: i64 = step_map
        .values()
        .filter_map(|s| s.iteration)
        .max()
        .unwrap_or(1);

    // Get current time for timeout detection
    let now_ms = chrono::Utc::now().timestamp_millis();
    // Steps running for more than 30 minutes are considered stuck
    const STEP_TIMEOUT_MS: i64 = 30 * 60 * 1000;

    // Determine which phases have completed steps (for stale detection of non-iterated phases)
    let has_completed_verification_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("verification")
            && (s.status == "success" || s.status == "failed")
    });
    let has_completed_agentic_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("agentic") && (s.status == "success" || s.status == "failed")
    });
    let has_completed_completion_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("completion") && (s.status == "success" || s.status == "failed")
    });

    // Fix stale "running" status for steps
    // A step is stale if:
    // 1. Its iteration has a verification_phase_result (iteration completed), OR
    // 2. Its iteration is less than the current max iteration (we've moved on), OR
    // 3. It has been running for more than 30 minutes (timeout), OR
    // 4. For setup steps without iteration: the loop has started (verification/agentic has run), OR
    // 5. For any step without iteration: a later phase has completed
    for step_data in step_map.values_mut() {
        if step_data.status == "running" {
            // Check for timeout
            let is_timed_out = step_data
                .start_time
                .map(|start| (now_ms - start) > STEP_TIMEOUT_MS)
                .unwrap_or(false);

            // Check for stale based on iteration or phase progression
            let is_stale = if let Some(iter) = step_data.iteration {
                // For steps with iteration: check iteration-based staleness
                completed_iterations.contains(&iter) || iter < max_iteration
            } else {
                // For steps without iteration: check phase-based staleness
                // Setup steps are stale if verification/agentic/completion has started
                // Verification/agentic steps without iteration are stale if completion has started
                match step_data.phase.as_deref() {
                    Some("setup") => {
                        // Setup is stale if loop has started or later phases have completed
                        max_iteration > 1
                            || has_completed_verification_step
                            || has_completed_agentic_step
                            || has_completed_completion_step
                    }
                    Some("verification") | Some("agentic") => {
                        // These phases normally have iterations, but if not, check if completion ran
                        has_completed_completion_step
                    }
                    Some("completion") => {
                        // Completion rarely gets stale, but timeout will catch it
                        false
                    }
                    _ => false,
                }
            };

            if is_stale || is_timed_out {
                // This iteration completed, we've moved past it, or it timed out.
                // Mark it as "failed" since something went wrong (no completion event).
                step_data.status = "failed".to_string();
                if step_data.error.is_none() {
                    if is_timed_out {
                        step_data.error =
                            Some("Step timed out (running for more than 30 minutes)".to_string());
                    } else {
                        step_data.error =
                            Some("Step did not complete properly (missing end event)".to_string());
                    }
                }
            }
        }
    }

    // Convert map to vector, sorted by start_time
    let mut executions: Vec<StepExecutionData> = step_map.into_values().collect();
    executions.sort_by(|a, b| {
        // Sort by start_time to maintain execution order
        a.start_time.cmp(&b.start_time)
    });

    // Determine current workflow stage from the most recent step event's phase
    // Priority: Find a step that is currently running, or fall back to the most recent step
    let current_stage: Option<String> = executions
        .iter()
        .filter(|e| e.status == "running")
        .filter_map(|e| e.phase.clone())
        .next_back()
        .or_else(|| {
            // Fall back to the most recent step's phase (by start_time, already sorted)
            executions
                .iter()
                .rev()
                .filter_map(|e| e.phase.clone())
                .next()
        });

    Ok(Json(serde_json::json!({
        "success": true,
        "task_run_id": task.id,
        "workflow_name": task.workflow_name,
        "workflow_type": task.workflow_type,
        "workflow_start_time": task.created_at,
        "current_stage": current_stage,
        "executions": executions,
        "count": executions.len()
    })))
}

/// Get screenshots for a task run from SQLite.
pub async fn get_task_run_screenshots(
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

    let screenshots = state.app_state.pg_db.get_task_run_screenshots(&id, None).await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "screenshots": screenshots,
        "count": screenshots.len()
    })))
}

/// Get execution spans from SQLite (tracing data).
///
/// Supports filtering by execution_id, name pattern, and minimum duration.
// TODO: Wire to PG when get_execution_spans PG wrapper is implemented
pub async fn get_execution_spans(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ExecutionSpansQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let execution_id = query.execution_id.as_deref().unwrap_or("");
    let all_spans = state
        .app_state
        .pg_db
        .get_execution_spans(execution_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply filters in memory since PG only supports execution_id filter
    let limit = query.limit.unwrap_or(100) as usize;
    let spans: Vec<_> = all_spans.into_iter()
        .filter(|s| {
            if let Some(ref pattern) = query.name_pattern {
                if let Some(name) = s.get("span_type").and_then(|v| v.as_str()) {
                    if !name.contains(pattern.trim_matches('%')) { return false; }
                }
            }
            if let Some(min_dur) = query.min_duration_ms {
                if let Some(dur) = s.get("duration_ms").and_then(|v| v.as_i64()) {
                    if dur < min_dur { return false; }
                }
            }
            true
        })
        .take(limit)
        .collect();

    Ok(Json(serde_json::json!({
        "spans": spans,
        "count": spans.len(),
        "filters": {
            "execution_id": query.execution_id,
            "name_pattern": query.name_pattern,
            "min_duration_ms": query.min_duration_ms,
            "limit": query.limit.unwrap_or(100)
        }
    })))
}

/// Migrate JSONL logs to SQLite for a task run.
pub async fn migrate_task_run_logs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use std::path::PathBuf;
    use tracing::info;

    info!("Migrating JSONL logs to SQLite for task run: {}", id);

    // Verify task exists
    let task_run = state
        .app_state
        .pg_db
        .get_task_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get the dev-logs directory path
    let dev_logs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(".dev-logs"))
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to resolve .dev-logs path".to_string(),
            )
        })?;

    // Log migration stub — CheckpointDb removed, log_migration not yet ported to PgDb
    let _task_run = task_run;
    let _dev_logs_dir = dev_logs_dir;
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Log migration not yet ported to PgDb".to_string(),
    ))
}

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
    steps.sort_by(|a, b| a.start_time.cmp(&b.start_time));

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
    let port = state.app_state.api_port.load(std::sync::atomic::Ordering::Relaxed);
    let running_tasks = state.app_state.pg_db.get_running_task_runs(Some(port)).await
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
    let events = state.app_state.pg_db.get_task_run_events(&task.id, None, None).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let aggregated = aggregate_step_events(&events);

    let completed_iterations = state.app_state.pg_db
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
    let usage = state.app_state.pg_db.get_phase_token_usage(&id).await
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
    let spans = state.app_state.pg_db.get_execution_spans(&trace_id).await
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
