//! Workflow state and resume endpoints extracted from task_runs.rs.
//!
//! Contains handlers for:
//! - Workflow state queries (get_workflow_state, get_full_workflow_state)
//! - Task output retrieval (get_task_output)
//! - Result data (get_task_run_result_data)
//! - Resume/restart (resume_task_run, compute_resume_point)

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::TaskRun;
use crate::mcp::types::ApiState;
use crate::unified_workflow_executor::extract_workflow_id_from_task_id;
use tauri::Manager;

/// PG-primary get_task_run helper.
async fn pg_get_task_run(state: &Arc<ApiState>, id: &str) -> Result<Option<TaskRun>, String> {
    state.app_state.pg_db.get_task_run(id).await
}

/// Response for workflow state endpoint.
#[derive(Debug, Serialize)]
pub struct WorkflowStateResponse {
    task_run_id: String,
    workflow_type: String,
    current_state: String,
    phase: Option<String>,
    iteration: Option<u32>,
    max_iterations: Option<u32>,
    is_complete: bool,
    is_stopped: bool,
    is_paused: bool,
    has_verification_plan: bool,
    workflow_start_time: String,
    state_data: Option<serde_json::Value>,
    /// Workflow stage for UI display (setup, verification, agentic, completion)
    workflow_stage: Option<String>,
    /// Human-readable display name for the workflow stage
    workflow_stage_display: Option<String>,
    /// Plan phase name (only for plan workflows)
    plan_phase_name: Option<String>,
    /// Plan phase index, zero-based (only for plan workflows)
    plan_phase_index: Option<usize>,
    /// Total number of plan phases (only for plan workflows)
    plan_total_phases: Option<usize>,
    /// Stage index for multi-stage workflows (0-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_index: Option<u32>,
    /// Total number of stages in a multi-stage workflow
    #[serde(skip_serializing_if = "Option::is_none")]
    total_stages: Option<usize>,
}

/// Map a workflow state name to a stage for UI display.
pub fn state_name_to_stage(state_name: &str) -> (Option<String>, Option<String>) {
    // State names are like: setup_running, setup_complete, verification_running, etc.
    let (stage, display) = if state_name.starts_with("setup") {
        ("setup", "Setup")
    } else if state_name.starts_with("verification") {
        ("verification", "Verification")
    } else if state_name.starts_with("agentic") {
        ("agentic", "Agentic")
    } else if state_name.starts_with("completion") {
        ("completion", "Completion")
    } else if state_name == "phase_running" || state_name == "phase_complete" {
        ("agentic", "Phase")
    } else if state_name == "sweep_running" || state_name == "sweep_complete" {
        ("completion", "Sweep")
    } else if state_name == "running" {
        // Legacy running state - no explicit stage
        return (None, None);
    } else {
        return (None, None);
    };
    (Some(stage.to_string()), Some(display.to_string()))
}

/// Get workflow state for a task run.
///
/// This endpoint provides explicit workflow state tracking that replaces
/// implicit state inference from task_runs status fields.
///
/// Works for all workflow types: unified, orchestrator, gui_automation.
pub async fn get_workflow_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<WorkflowStateResponse>, (StatusCode, String)> {
    // First get the task run for metadata
    let task_run = pg_get_task_run(&state, &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Try to get explicit workflow state
    let explicit_state = state
        .app_state
        .pg_db
        .get_workflow_execution_state(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Try to get workflow definition for max_iterations and verification info
    let workflow_def = if let Some(ref workflow_name) = task_run.workflow_name {
        state
            .app_state
            .pg_db
            .get_unified_workflow_by_name(workflow_name)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    // `max_iterations` is already `Option<u32>` on UnifiedWorkflow (None =
    // unlimited). The outer `map` adds a "workflow not found" case, giving
    // `Option<Option<u32>>`. Flatten so the caller sees a single Option.
    let max_iterations = workflow_def.as_ref().and_then(|w| w.max_iterations);

    // Check if a verification plan exists:
    // 1. Unified workflows: check if verification_steps is non-empty
    // 2. Orchestrator workflows: check if a verification_plan record exists
    let has_verification_plan = workflow_def
        .as_ref()
        .map(|w| !w.verification_steps.is_empty())
        .unwrap_or(false)
        || state
            .app_state
            .pg_db
            .get_latest_verification_plan(&id)
            .await
            .ok()
            .flatten()
            .is_some();

    if let Some(ws) = explicit_state {
        // Return explicit state
        let state_data: Option<serde_json::Value> = ws
            .state_data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let is_terminal = matches!(
            ws.state_name.as_str(),
            "completion_complete" | "failed" | "stopped"
        );
        let is_stopped = ws.state_name == "stopped";
        let is_paused = ws.state_name == "paused" || task_run.status == "paused";

        // Map state name to stage for UI
        let (workflow_stage, workflow_stage_display) = state_name_to_stage(&ws.state_name);

        // Extract plan phase details from state_data for plan workflows.
        // State data is serialized as e.g. {"PhaseRunning":{"phase_index":0,"phase_name":"Fix API"}}
        let (plan_phase_name, plan_phase_index, plan_total_phases) = if ws.workflow_type == "plan" {
            // Try to extract from any variant that has phase_index/phase_name
            let inner = state_data.as_ref().and_then(|d| {
                d.get("PhaseRunning")
                    .or_else(|| d.get("PhaseComplete"))
                    .or_else(|| d.get("Failed"))
                    .or_else(|| d.get("Stopped"))
            });
            let phase_name = inner
                .and_then(|d| d.get("phase_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let phase_index = inner
                .and_then(|d| d.get("phase_index"))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let total_phases = task_run.max_sessions.map(|m| m as usize);
            (phase_name, phase_index, total_phases)
        } else {
            (None, None, None)
        };

        // Extract stage_index from state_data for multi-stage workflows
        let stage_index = extract_stage_index(&state_data);
        let total_stages = workflow_def
            .as_ref()
            .map(|w| w.stages.len())
            .filter(|&n| n > 0);

        Ok(Json(WorkflowStateResponse {
            task_run_id: id,
            workflow_type: ws.workflow_type,
            current_state: ws.state_name,
            phase: ws.phase,
            iteration: ws.iteration,
            max_iterations,
            is_complete: is_terminal,
            is_stopped,
            is_paused,
            has_verification_plan,
            workflow_start_time: task_run.created_at,
            state_data,
            workflow_stage,
            workflow_stage_display,
            plan_phase_name,
            plan_phase_index,
            plan_total_phases,
            stage_index,
            total_stages,
        }))
    } else {
        // Fallback: Infer state from task_run for backward compatibility
        let workflow_type = task_run
            .workflow_type
            .clone()
            .unwrap_or_else(|| "legacy_session".to_string());

        let (current_state, is_complete, is_stopped, is_paused) = match task_run.status.as_str() {
            "running" => ("running".to_string(), false, false, false),
            "complete" => ("complete".to_string(), true, false, false),
            "failed" => ("failed".to_string(), true, false, false),
            "stopped" => ("stopped".to_string(), true, true, false),
            "paused" => ("paused".to_string(), false, false, true),
            other => (other.to_string(), false, false, false),
        };

        Ok(Json(WorkflowStateResponse {
            task_run_id: id,
            workflow_type,
            current_state,
            phase: None,
            iteration: None,
            max_iterations,
            is_complete,
            is_stopped,
            is_paused,
            has_verification_plan,
            workflow_start_time: task_run.created_at,
            state_data: None,
            workflow_stage: None,
            workflow_stage_display: None,
            plan_phase_name: None,
            plan_phase_index: None,
            plan_total_phases: None,
            stage_index: None,
            total_stages: None,
        }))
    }
}

// =============================================================================
// Result Data (generated_workflow_id, etc.)
// =============================================================================

/// Get the result_data JSON for a task run.
///
/// Returns the JSON object stored in task_runs.result_data (e.g. generated_workflow_id
/// from workflow generation). Returns empty object {} if no result_data is set.
pub async fn get_task_run_result_data(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let result_data = state
        .app_state
        .pg_db
        .get_task_run_result_data(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    match result_data {
        Some(json_str) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&json_str).unwrap_or(serde_json::json!({}));
            Ok(Json(parsed))
        }
        None => Ok(Json(serde_json::json!({}))),
    }
}

// =============================================================================
// Full Workflow State (for frontend restart recovery)
// =============================================================================

/// Full workflow state response for restart recovery.
///
/// This endpoint returns authoritative state from the database, including:
/// - Orchestrator state (phase, iteration, state name)
/// - All step checkpoints (which steps completed)
/// - Progress markers for the current step (intra-step progress)
/// - Resume point (where execution will continue)
#[derive(Debug, Serialize)]
pub struct FullWorkflowStateResponse {
    /// Task run metadata
    task_run: TaskRunSummary,
    /// Orchestrator state from workflow_execution_state table
    orchestrator_state: Option<OrchestratorStateData>,
    /// All step checkpoints for this execution
    checkpoints: Vec<StepCheckpointData>,
    /// Progress marker for the currently running step (if any)
    current_step_progress: Option<StepProgressData>,
    /// Computed resume point
    resume_point: ResumePointData,
    /// Phases defined in the workflow (e.g., ["setup", "verification", "agentic", "completion"])
    defined_phases: Vec<String>,
    /// Stage names for multi-stage workflows (ordered). Empty if no stages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stage_names: Vec<String>,
}

/// Task run summary for full state response.
#[derive(Debug, Serialize)]
pub struct TaskRunSummary {
    id: String,
    status: String,
    task_name: String,
    workflow_type: Option<String>,
    workflow_name: Option<String>,
    started_at: String,
    sessions_count: u32,
    max_sessions: Option<u32>,
}

/// Orchestrator state data from workflow_execution_state table.
#[derive(Debug, Serialize)]
pub struct OrchestratorStateData {
    state_name: String,
    state_data: Option<serde_json::Value>,
    phase: Option<String>,
    iteration: Option<u32>,
    updated_at: String,
    /// Mapped workflow stage for UI display
    workflow_stage: Option<String>,
    workflow_stage_display: Option<String>,
    /// Stage index for multi-stage workflows (0-indexed, from state_data)
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_index: Option<u32>,
}

/// Step checkpoint data for full state response.
#[derive(Debug, Serialize)]
pub struct StepCheckpointData {
    id: String,
    execution_id: String,
    phase: String,
    iteration: Option<u32>,
    step_index: usize,
    stage_index: Option<u32>,
    step_type: String,
    step_name: Option<String>,
    status: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

/// Step progress marker data for full state response.
#[derive(Debug, Serialize)]
pub struct StepProgressData {
    checkpoint_id: String,
    marker_type: String,
    current_value: u64,
    total_value: Option<u64>,
    description: Option<String>,
    data_json: Option<serde_json::Value>,
    created_at: String,
}

/// Resume point data for full state response.
#[derive(Debug, Serialize)]
pub struct ResumePointData {
    /// Type of resume point
    #[serde(rename = "type")]
    resume_type: String,
    /// Iteration number (for verification/agentic phases)
    iteration: Option<u32>,
    /// Step index to resume from
    from_step: Option<usize>,
    /// Human-readable description
    description: String,
}

/// Extract stage_index from the state_data JSON.
///
/// The state_data is a serde internally-tagged enum (tag = "type", rename_all = "snake_case"),
/// so the JSON looks like: `{ "type": "verification_running", "iteration": 1, "stage_index": 2 }`
/// The stage_index field is at the top level of the object, not nested inside a variant key.
fn extract_stage_index(state_data: &Option<serde_json::Value>) -> Option<u32> {
    let data = state_data.as_ref()?;
    data.get("stage_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
}

/// Get full workflow state for restart recovery.
///
/// This endpoint returns authoritative state from the database for the frontend
/// to use when recovering from a restart. It consolidates data from:
/// - task_runs table
/// - workflow_execution_state table
/// - workflow_step_checkpoints table
/// - step_progress_markers table
pub async fn get_full_workflow_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<FullWorkflowStateResponse>, (StatusCode, String)> {
    // Get task run
    let task_run = pg_get_task_run(&state, &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get orchestrator state
    let explicit_state = state
        .app_state
        .pg_db
        .get_workflow_execution_state(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let orchestrator_state = explicit_state.map(|ws| {
        let state_data: Option<serde_json::Value> = ws
            .state_data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());
        let stage_index = extract_stage_index(&state_data);
        let (workflow_stage, workflow_stage_display) = state_name_to_stage(&ws.state_name);

        OrchestratorStateData {
            state_name: ws.state_name,
            state_data,
            phase: ws.phase,
            iteration: ws.iteration,
            updated_at: ws.updated_at,
            workflow_stage,
            workflow_stage_display,
            stage_index,
        }
    });

    // Get all step checkpoints
    let checkpoints_raw = state
        .app_state
        .pg_db
        .get_all_workflow_step_checkpoints(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let checkpoints: Vec<StepCheckpointData> = checkpoints_raw
        .iter()
        .map(|cp| StepCheckpointData {
            id: cp.id.clone(),
            execution_id: cp.execution_id.clone(),
            phase: cp.phase.clone(),
            iteration: cp.iteration,
            step_index: cp.step_index,
            stage_index: cp.stage_index,
            step_type: cp.step_type.clone(),
            step_name: cp.step_name.clone(),
            status: cp.status.to_string(),
            started_at: cp.started_at.clone(),
            completed_at: cp.completed_at.clone(),
            duration_ms: cp.duration_ms,
            error: cp.error.clone(),
        })
        .collect();

    // Get current step progress
    let progress_raw = state
        .app_state
        .pg_db
        .get_current_step_progress(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let current_step_progress = progress_raw.map(|p| StepProgressData {
        checkpoint_id: p.checkpoint_id,
        marker_type: p.marker_type,
        current_value: p.current_value,
        total_value: p.total_value,
        description: p.description,
        data_json: p
            .data_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        created_at: p.created_at,
    });

    // Compute resume point from the data we've already loaded above.
    let resume_point =
        compute_resume_point(&orchestrator_state, &checkpoints, &current_step_progress);

    // Determine defined phases from the workflow definition
    let defined_phases = if task_run.workflow_type.as_deref() == Some("unified") {
        if let Some(ref wn) = task_run.workflow_name {
            match state.app_state.pg_db.get_unified_workflow_by_name(wn).await {
                Ok(Some(wf)) => {
                    let mut phases = Vec::new();
                    if !wf.setup_steps.is_empty() {
                        phases.push("setup".to_string());
                    }
                    if !wf.verification_steps.is_empty() {
                        phases.push("verification".to_string());
                    }
                    if !wf.agentic_steps.is_empty() {
                        phases.push("agentic".to_string());
                    }
                    if !wf.completion_steps.is_empty() {
                        phases.push("completion".to_string());
                    }
                    phases
                }
                _ => vec!["setup", "verification", "agentic", "completion"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            }
        } else {
            vec!["setup", "verification", "agentic", "completion"]
                .into_iter()
                .map(String::from)
                .collect()
        }
    } else {
        vec![]
    };

    // Get stage names from workflow definition
    let stage_names: Vec<String> = extract_workflow_id_from_task_id(&id)
        .and_then(|wf_id| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(state.app_state.pg_db.get_unified_workflow(&wf_id))
            })
            .ok()
            .flatten()
        })
        .map(|wf| wf.stages.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();

    Ok(Json(FullWorkflowStateResponse {
        task_run: TaskRunSummary {
            id: task_run.id,
            status: task_run.status,
            task_name: task_run.task_name,
            workflow_type: task_run.workflow_type,
            workflow_name: task_run.workflow_name,
            started_at: task_run.created_at,
            sessions_count: task_run.sessions_count,
            max_sessions: task_run.max_sessions,
        },
        orchestrator_state,
        checkpoints,
        current_step_progress,
        resume_point,
        defined_phases,
        stage_names,
    }))
}

/// Compute the resume point for a workflow execution from already-loaded data.
///
/// This is a pure function — it does no DB I/O. The caller
/// (`get_full_workflow_state`) fetches the checkpoints and progress marker up
/// front and passes them in.
///
/// Returns one of three `resume_type` values:
/// - `"from_start"` — no checkpoints exist for this execution
/// - `"in_progress_step"` — the latest checkpoint is still running and has
///   progress markers (resume continues that step mid-way)
/// - `"from_step"` — there is at least one checkpoint; resume from after the
///   last successful step (or retry the last failed/stopped step)
pub fn compute_resume_point(
    orchestrator_state: &Option<OrchestratorStateData>,
    checkpoints: &[StepCheckpointData],
    current_progress: &Option<StepProgressData>,
) -> ResumePointData {
    // Helper: readable label for a step ("step 3 (verification)" / "step 3").
    fn step_label(cp: &StepCheckpointData) -> String {
        if let Some(name) = cp.step_name.as_deref().filter(|s| !s.is_empty()) {
            format!("step {} ({})", cp.step_index + 1, name)
        } else {
            format!("step {} ({})", cp.step_index + 1, cp.step_type)
        }
    }

    // Orchestrator-reported iteration is the fallback when a checkpoint
    // doesn't carry one.
    let fallback_iter = orchestrator_state.as_ref().and_then(|os| os.iteration);

    if checkpoints.is_empty() {
        return ResumePointData {
            resume_type: "from_start".to_string(),
            iteration: fallback_iter,
            from_step: None,
            description: "Will restart from the beginning.".to_string(),
        };
    }

    // Find the latest checkpoint by (iteration desc, step_index desc).
    // iteration=None sorts as 0 so rows with iterations win ties.
    let latest = checkpoints
        .iter()
        .max_by_key(|cp| (cp.iteration.unwrap_or(0), cp.step_index))
        .expect("checkpoints non-empty");

    let latest_iter = latest.iteration.or(fallback_iter);
    let status = latest.status.as_str();

    // In-progress: the latest checkpoint is still running and we have an
    // intra-step progress marker — resume mid-step.
    if status == "running" {
        if let Some(progress) = current_progress.as_ref() {
            let progress_blurb = match progress.total_value {
                Some(total) => format!(
                    "{}/{} {}",
                    progress.current_value, total, progress.marker_type
                ),
                None => format!("{} {}", progress.current_value, progress.marker_type),
            };
            let iter_blurb = match latest_iter {
                Some(i) => format!(" (iteration {})", i),
                None => String::new(),
            };
            return ResumePointData {
                resume_type: "in_progress_step".to_string(),
                iteration: latest_iter,
                from_step: Some(latest.step_index),
                description: format!(
                    "Resume mid-{}{}: {} complete.",
                    step_label(latest),
                    iter_blurb,
                    progress_blurb
                ),
            };
        }
        // Running but no progress marker — treat like a retry.
        let iter_blurb = match latest_iter {
            Some(i) => format!(" in iteration {}", i),
            None => String::new(),
        };
        return ResumePointData {
            resume_type: "from_step".to_string(),
            iteration: latest_iter,
            from_step: Some(latest.step_index),
            description: format!(
                "Retry {}{} (previous attempt did not finish).",
                step_label(latest),
                iter_blurb
            ),
        };
    }

    // Success / skipped: resume from the step after the latest one.
    if matches!(status, "success" | "skipped") {
        let next_step = latest.step_index + 1;
        let iter_blurb = match latest_iter {
            Some(i) => format!(" in iteration {}", i),
            None => String::new(),
        };
        return ResumePointData {
            resume_type: "from_step".to_string(),
            iteration: latest_iter,
            from_step: Some(next_step),
            description: format!(
                "Resume from step {}{} (after {}).",
                next_step + 1,
                iter_blurb,
                step_label(latest)
            ),
        };
    }

    // Failed / pending / unknown: retry the latest step.
    let iter_blurb = match latest_iter {
        Some(i) => format!(" in iteration {}", i),
        None => String::new(),
    };
    let reason = if status == "failed" {
        " (previous attempt failed)"
    } else {
        ""
    };
    ResumePointData {
        resume_type: "from_step".to_string(),
        iteration: latest_iter,
        from_step: Some(latest.step_index),
        description: format!("Retry {}{}{}.", step_label(latest), iter_blurb, reason),
    }
}

#[cfg(test)]
mod resume_point_tests {
    use super::*;

    fn cp(
        step_index: usize,
        iteration: Option<u32>,
        status: &str,
        step_type: &str,
        step_name: Option<&str>,
    ) -> StepCheckpointData {
        StepCheckpointData {
            id: format!("cp-{}", step_index),
            execution_id: "exec".to_string(),
            phase: "agentic".to_string(),
            iteration,
            step_index,
            stage_index: None,
            step_type: step_type.to_string(),
            step_name: step_name.map(String::from),
            status: status.to_string(),
            started_at: None,
            completed_at: None,
            duration_ms: None,
            error: None,
        }
    }

    fn orch_state(iteration: Option<u32>) -> Option<OrchestratorStateData> {
        Some(OrchestratorStateData {
            state_name: "agentic_running".to_string(),
            state_data: None,
            phase: Some("agentic".to_string()),
            iteration,
            updated_at: "2026-04-07T00:00:00Z".to_string(),
            workflow_stage: Some("agentic".to_string()),
            workflow_stage_display: Some("Agentic".to_string()),
            stage_index: None,
        })
    }

    #[test]
    fn empty_checkpoints_is_from_start() {
        let rp = compute_resume_point(&None, &[], &None);
        assert_eq!(rp.resume_type, "from_start");
        assert!(rp.from_step.is_none());
        assert!(rp.description.contains("beginning"));
        // Description must NOT contain the old SQLite leak.
        assert!(!rp.description.to_lowercase().contains("sqlite"));
    }

    #[test]
    fn from_start_uses_orchestrator_iteration_fallback() {
        let rp = compute_resume_point(&orch_state(Some(3)), &[], &None);
        assert_eq!(rp.iteration, Some(3));
    }

    #[test]
    fn completed_checkpoints_resume_from_next_step() {
        let cps = vec![
            cp(0, Some(1), "success", "prompt", Some("Setup")),
            cp(1, Some(1), "success", "verification", Some("Verify")),
            cp(2, Some(1), "success", "agentic", Some("Agentic run")),
        ];
        let rp = compute_resume_point(&orch_state(Some(1)), &cps, &None);
        assert_eq!(rp.resume_type, "from_step");
        assert_eq!(rp.from_step, Some(3));
        assert_eq!(rp.iteration, Some(1));
        assert!(rp.description.contains("Resume from step 4"));
    }

    #[test]
    fn running_checkpoint_with_progress_is_in_progress_step() {
        let cps = vec![
            cp(0, Some(2), "success", "prompt", None),
            cp(1, Some(2), "running", "verification", Some("Verify")),
        ];
        let progress = Some(StepProgressData {
            checkpoint_id: "cp-1".to_string(),
            marker_type: "actions".to_string(),
            current_value: 7,
            total_value: Some(12),
            description: None,
            data_json: None,
            created_at: "2026-04-07T00:00:00Z".to_string(),
        });
        let rp = compute_resume_point(&orch_state(Some(2)), &cps, &progress);
        assert_eq!(rp.resume_type, "in_progress_step");
        assert_eq!(rp.from_step, Some(1));
        assert_eq!(rp.iteration, Some(2));
        assert!(rp.description.contains("7/12 actions"));
        assert!(rp.description.contains("step 2"));
    }

    #[test]
    fn running_checkpoint_without_progress_is_retry_from_step() {
        let cps = vec![cp(4, Some(1), "running", "shell_command", None)];
        let rp = compute_resume_point(&orch_state(Some(1)), &cps, &None);
        assert_eq!(rp.resume_type, "from_step");
        assert_eq!(rp.from_step, Some(4));
        assert!(rp.description.contains("Retry"));
    }

    #[test]
    fn failed_checkpoint_retries_from_same_step() {
        let cps = vec![
            cp(0, Some(1), "success", "prompt", None),
            cp(1, Some(1), "failed", "verification", Some("Verify")),
        ];
        let rp = compute_resume_point(&orch_state(Some(1)), &cps, &None);
        assert_eq!(rp.resume_type, "from_step");
        assert_eq!(rp.from_step, Some(1));
        assert!(rp.description.contains("Retry"));
        assert!(rp.description.contains("previous attempt failed"));
    }

    #[test]
    fn latest_is_highest_iteration_then_highest_step() {
        // Iteration 2 step 0 should beat iteration 1 step 9.
        let cps = vec![
            cp(9, Some(1), "success", "prompt", None),
            cp(0, Some(2), "success", "verification", None),
        ];
        let rp = compute_resume_point(&orch_state(Some(2)), &cps, &None);
        assert_eq!(rp.iteration, Some(2));
        assert_eq!(rp.from_step, Some(1)); // 0 + 1
    }

    #[test]
    fn description_never_mentions_sqlite() {
        // Regression: the old stub leaked "SQLite removed" into the UI.
        let variants: Vec<Vec<StepCheckpointData>> = vec![
            vec![],
            vec![cp(0, Some(1), "success", "prompt", None)],
            vec![cp(0, Some(1), "failed", "prompt", None)],
            vec![cp(0, Some(1), "running", "prompt", None)],
        ];
        for cps in variants {
            let rp = compute_resume_point(&orch_state(Some(1)), &cps, &None);
            assert!(!rp.description.to_lowercase().contains("sqlite"));
            assert!(!rp.description.to_lowercase().contains("unavailable"));
        }
    }
}

/// Query params for getting task output.
#[derive(Debug, Deserialize)]
pub struct TaskOutputQuery {
    /// Number of characters from end of output to return (optional)
    tail_chars: Option<usize>,
}

/// Get task output (optionally just the tail).
pub async fn get_task_output(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TaskOutputQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // First verify task exists
    let task_run = pg_get_task_run(&state, &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let output = if let Some(tail_chars) = query.tail_chars {
        state
            .app_state
            .pg_db
            .get_task_output_tail(&id, tail_chars)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run.output_log
    };

    Ok(Json(serde_json::json!({
        "id": id,
        "output": output,
        "status": task_run.status,
        "sessions_count": task_run.sessions_count
    })))
}

/// Request body for resuming a task run.
#[derive(Debug, Deserialize)]
pub struct ResumeTaskRunRequest {
    /// Additional sessions to add (if not already reopened). Default: 1
    additional_sessions: Option<u32>,
}

/// Resume a task run that was previously completed or interrupted.
///
/// This endpoint combines reopening the task in the database with actually
/// triggering the workflow execution. It handles:
/// 1. Reopening the task if it's not already running
/// 2. Extracting the workflow ID from the task ID
/// 3. Starting LoopController to execute the workflow
pub async fn resume_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ResumeTaskRunRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use crate::unified_workflow_executor::{
        extract_workflow_id_from_task_id, LoopConfig, LoopController,
    };

    info!("Resume task run request: {}", id);

    // Get the task run
    let task_run = pg_get_task_run(&state, &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // If not already running, reopen it
    let task_run = if task_run.status != "running" {
        let additional_sessions = request.additional_sessions.unwrap_or(1);
        state
            .app_state
            .pg_db
            .reopen_task_run(&id, additional_sessions)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run
    };

    // Try to get the workflow definition - first by extracting ID from task ID, then by name
    let (workflow_id, workflow) = if let Some(wf_id) = extract_workflow_id_from_task_id(&id) {
        // Old format: unified-workflow-{uuid}-{timestamp}
        let wf = state
            .app_state
            .pg_db
            .get_unified_workflow(&wf_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Workflow definition not found: {}", wf_id),
                )
            })?;
        (wf_id, wf)
    } else if let Some(ref wf_name) = task_run.workflow_name {
        // Look up workflow by name — try exact match first, then strip
        // reflection prefixes ("Reflection: ", "Project Reflection: ") since
        // reflection task runs store a prefixed name while the workflow
        // definition keeps the original name.
        let wf = state
            .app_state
            .pg_db
            .get_unified_workflow_by_name(wf_name)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .or_else(|| {
                let stripped = wf_name
                    .strip_prefix("Project Reflection: ")
                    .or_else(|| wf_name.strip_prefix("Reflection: "));
                stripped.and_then(|name| {
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(state.app_state.pg_db.get_unified_workflow_by_name(name))
                    })
                    .ok()
                    .flatten()
                })
            })
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Workflow definition not found with name: {}", wf_name),
                )
            })?;
        (wf.id.clone(), wf)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Cannot determine workflow for task ID '{}'. Task has no workflow_name and ID format is not recognized.",
                id
            ),
        ));
    };

    info!(
        "Resuming workflow '{}' (task_id: {}, workflow_id: {}, iteration: {})",
        task_run.task_name,
        id,
        workflow_id,
        task_run.sessions_count + 1
    );

    // Normalize to stages — all workflows are now multi-stage
    let normalized_stages = workflow.normalize_to_stages();
    let total_stages = normalized_stages.len();

    // Convert each stage to StageConfig
    let stages: Vec<crate::unified_workflow_executor::StageConfig> = normalized_stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| {
            crate::unified_workflows::stage_to_stage_config(
                stage,
                idx,
                total_stages,
                workflow.preflight_check_enabled,
                workflow.log_watch_enabled,
                workflow.health_check_enabled,
                &workflow.health_check_urls,
            )
        })
        .collect();

    // Build combined prompt from all agentic steps across all stages
    let combined_prompt = normalized_stages
        .iter()
        .flat_map(|stage| stage.agentic_steps.iter())
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    // Calculate starting iteration for resume (sessions_count is the number of completed iterations)
    let starting_iteration = task_run.sessions_count;

    // For error-fix workflows, run agentic first (only if starting fresh)
    let run_agentic_first = !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

    let loop_config = LoopConfig {
        max_iterations: workflow.iter_cap(),
        base_prompt: combined_prompt,
        workflow_name: task_run.task_name.clone(),
        workflow_id: workflow_id.clone(),
        execution_id: id.clone(),
        targeted_error_ids: workflow.targeted_error_ids.clone(),
        starting_iteration,
        run_agentic_first,
        artifact_dir: None,
        is_dev_mode: cfg!(debug_assertions),
        enable_sweep: workflow.enable_sweep,
        max_sweep_iterations: workflow.max_sweep_iterations,
        stages,
        stop_on_failure: workflow.stop_on_failure,
        constraint_overrides: workflow.constraint_overrides.clone(),
        reflection_mode: workflow.reflection_mode,
        provider_override: None,
        model_override: None,
        model_overrides: workflow.model_overrides.clone(),
        stage_index: None,
        max_sessions: workflow.max_iterations,
        auto_run_generated: false,
        approval_gate: workflow.approval_gate,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 100_000,
        enforce_token_budget: workflow.enforce_token_budget,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: workflow.acceptance_criteria.clone(),
        multi_agent_mode: false,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
        max_fix_attempts: workflow.max_fix_attempts,
        max_ci_auto_resumes: workflow.max_ci_auto_resumes,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    };

    // Spawn the workflow execution in background with panic protection
    let app_state = state.app_state.clone();
    let config_storage = state.config_storage.clone();
    let app_handle = state.app_handle.clone();
    let pid_tracker = state.current_ai_pids.clone();
    let task_name = task_run.task_name.clone();
    let execution_id_for_guard = id.clone();

    // Get session manager for interactive mode
    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    if crate::restate::launch::should_use_restate(&restate_settings).await {
        match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
            Ok(input) => {
                if let Err(e) = state
                    .app_state
                    .pg_db
                    .save_restate_workflow_execution(
                        &execution_id_for_guard,
                        &execution_id_for_guard,
                        None,
                    )
                    .await
                {
                    tracing::error!("Failed to record Restate workflow: {}", e);
                }

                if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                    &execution_id_for_guard,
                    &input,
                    &restate_settings.ingress_url(),
                )
                .await
                {
                    tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                } else {
                    use_legacy = false;
                }
            }
            Err(e) => {
                tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
            }
        }
    }

    // Use panic-safe spawning to ensure task is marked as failed if workflow panics
    if use_legacy {
        let url_lock = Some(state.app_state.url_lock_manager.clone());
        let file_registry = Some(state.app_state.file_registry_manager.clone());
        let file_lock = Some(state.app_state.file_lock_manager.clone());
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            execution_id_for_guard,
            task_name.clone(),
            url_lock,
            file_registry,
            file_lock,
            state.app_state.pg_db.clone(),
            async move {
                let mut controller =
                    LoopController::new(app_state, config_storage, app_handle, pid_tracker)
                        .with_session_manager(session_manager);

                controller
                    .run(
                        loop_config,
                        Vec::new(), // No top-level steps — all in stages
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )
                    .await
            },
        );
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Task run '{}' resumed successfully", task_run.task_name),
        "task_run_id": id,
        "workflow_id": workflow_id,
        "iteration": task_run.sessions_count + 1
    })))
}
