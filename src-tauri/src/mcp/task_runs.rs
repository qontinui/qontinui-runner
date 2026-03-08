//! Task run handlers for MCP API
//!
//! Provides HTTP handlers for task run management:
//! CRUD, workflow state, execution control, event queries,
//! verification results, knowledge, screenshots, and more.

use axum::{extract::State, http::StatusCode, response::sse::Sse, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::database::{CreateTaskRunInput, TaskRun};
use crate::mcp::shared::emit_ai_output;
use crate::mcp::types::ApiState;
use crate::safe_lock::safe_lock_or_recover;
use crate::summary_generator;
use crate::unified_workflow_executor::extract_workflow_id_from_task_id;
use tauri::Manager;

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

    tokio::task::spawn_blocking(move || {
        db.get_recent_task_runs_filtered(limit, workflow_type.as_deref())
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

    tokio::task::spawn_blocking(move || db.get_running_task_runs())
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
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Try to get explicit workflow state
    let explicit_state = state
        .app_state
        .checkpoint_db
        .get_workflow_execution_state(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Try to get workflow definition for max_iterations and verification info
    let workflow_def = if let Some(ref workflow_name) = task_run.workflow_name {
        state
            .app_state
            .checkpoint_db
            .get_unified_workflow_by_name(workflow_name)
            .ok()
            .flatten()
    } else {
        None
    };
    let max_iterations = workflow_def.as_ref().map(|w| w.max_iterations);

    // Check if a verification plan exists:
    // 1. Unified workflows: check if verification_steps is non-empty
    // 2. Orchestrator workflows: check if a verification_plan record exists
    let has_verification_plan = workflow_def
        .as_ref()
        .map(|w| !w.verification_steps.is_empty())
        .unwrap_or(false)
        || state
            .app_state
            .checkpoint_db
            .get_latest_verification_plan(&id)
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
        let is_paused = ws.state_name == "paused";

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
        .checkpoint_db
        .get_task_run_result_data(&id)
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
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get orchestrator state
    let explicit_state = state
        .app_state
        .checkpoint_db
        .get_workflow_execution_state(&id)
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
        .checkpoint_db
        .get_all_workflow_step_checkpoints(&id)
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
        .checkpoint_db
        .get_current_step_progress(&id)
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

    // Compute resume point using ResumeManager
    let resume_point = compute_resume_point(
        state.app_state.checkpoint_db.clone(),
        &id,
        &orchestrator_state,
    );

    // Determine defined phases from the workflow definition
    let defined_phases = if task_run.workflow_type.as_deref() == Some("unified") {
        if let Some(ref wn) = task_run.workflow_name {
            match state
                .app_state
                .checkpoint_db
                .get_unified_workflow_by_name(wn)
            {
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
            state
                .app_state
                .checkpoint_db
                .get_unified_workflow(&wf_id)
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

/// Compute the resume point for a workflow execution.
pub fn compute_resume_point(
    db: std::sync::Arc<crate::database::CheckpointDb>,
    execution_id: &str,
    orchestrator_state: &Option<OrchestratorStateData>,
) -> ResumePointData {
    // Try to use the ResumeManager for accurate resume point calculation
    use crate::unified_workflow_executor::ResumeManager;

    let resume_mgr = ResumeManager::new(db);
    match resume_mgr.determine_resume_point(execution_id) {
        Ok(resume_point) => {
            use crate::unified_workflow_executor::ResumePoint;
            match resume_point {
                ResumePoint::FromStart => ResumePointData {
                    resume_type: "from_start".to_string(),
                    iteration: None,
                    from_step: None,
                    description: resume_point.description(),
                },
                ResumePoint::SetupPhase { from_step, .. } => ResumePointData {
                    resume_type: "setup_phase".to_string(),
                    iteration: None,
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
                ResumePoint::VerificationPhase {
                    iteration,
                    from_step,
                    ..
                } => ResumePointData {
                    resume_type: "verification_phase".to_string(),
                    iteration: Some(iteration),
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
                ResumePoint::AgenticPhase { iteration, .. } => ResumePointData {
                    resume_type: "agentic_phase".to_string(),
                    iteration: Some(iteration),
                    from_step: None,
                    description: resume_point.description(),
                },
                ResumePoint::CompletionPhase { from_step } => ResumePointData {
                    resume_type: "completion_phase".to_string(),
                    iteration: None,
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
                ResumePoint::StageStart { from_stage } => ResumePointData {
                    resume_type: "stage_start".to_string(),
                    iteration: None,
                    from_step: Some(from_stage as usize),
                    description: resume_point.description(),
                },
                ResumePoint::ApprovalPhase { iteration, .. } => ResumePointData {
                    resume_type: "approval_phase".to_string(),
                    iteration: Some(iteration),
                    from_step: None,
                    description: resume_point.description(),
                },
            }
        }
        Err(e) => {
            // Fall back to basic inference from orchestrator state
            warn!("Failed to determine resume point: {}, using fallback", e);
            if let Some(orch) = orchestrator_state {
                ResumePointData {
                    resume_type: orch.phase.clone().unwrap_or_else(|| "unknown".to_string()),
                    iteration: orch.iteration,
                    from_step: Some(0),
                    description: format!("Resume from {} (fallback)", orch.state_name),
                }
            } else {
                ResumePointData {
                    resume_type: "from_start".to_string(),
                    iteration: None,
                    from_step: None,
                    description: "No state found, start from beginning".to_string(),
                }
            }
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
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let output = if let Some(tail_chars) = query.tail_chars {
        state
            .app_state
            .checkpoint_db
            .get_task_output_tail(&id, tail_chars)
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

/// Stop a running task run.
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
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // If not already running, reopen it
    let task_run = if task_run.status != "running" {
        let additional_sessions = request.additional_sessions.unwrap_or(1);
        state
            .app_state
            .checkpoint_db
            .reopen_task_run(&id, additional_sessions)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run
    };

    // Try to get the workflow definition - first by extracting ID from task ID, then by name
    let (workflow_id, workflow) = if let Some(wf_id) = extract_workflow_id_from_task_id(&id) {
        // Old format: unified-workflow-{uuid}-{timestamp}
        let wf = state
            .app_state
            .checkpoint_db
            .get_unified_workflow(&wf_id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Workflow definition not found: {}", wf_id),
                )
            })?;
        (wf_id, wf)
    } else if let Some(ref wf_name) = task_run.workflow_name {
        // New format: composed-run-{timestamp}-workflow-{n}
        // Look up workflow by name instead
        let wf = state
            .app_state
            .checkpoint_db
            .get_unified_workflow_by_name(wf_name)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
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
        max_iterations: workflow.max_iterations,
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
        reflection_mode: workflow.reflection_mode,
        provider_override: None,
        model_override: None,
        model_overrides: workflow.model_overrides.clone(),
        stage_index: None,
        max_sessions: Some(workflow.max_iterations),
        auto_run_generated: false,
        approval_gate: workflow.approval_gate,
        max_context_tokens: 100_000,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
    };

    // Spawn the workflow execution in background with panic protection
    let app_state = state.app_state.clone();
    let config_storage = state.config_storage.clone();
    let app_handle = state.app_handle.clone();
    let pid_tracker = state.current_ai_pids.clone();
    let checkpoint_db = state.app_state.checkpoint_db.clone();
    let task_name = task_run.task_name.clone();
    let execution_id_for_guard = id.clone();

    // Get session manager for interactive mode
    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    // Use panic-safe spawning to ensure task is marked as failed if workflow panics
    let url_lock = Some(state.app_state.url_lock_manager.clone());
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        checkpoint_db,
        execution_id_for_guard,
        task_name.clone(),
        url_lock,
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

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Task run '{}' resumed successfully", task_run.task_name),
        "task_run_id": id,
        "workflow_id": workflow_id,
        "iteration": task_run.sessions_count + 1
    })))
}

/// Query parameters for task run events.
#[derive(Debug, Deserialize)]
pub struct TaskRunEventsQuery {
    event_type: Option<String>,
    limit: Option<u32>,
}

/// Get events for a task run from SQLite (hybrid logging).
pub async fn get_task_run_events(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TaskRunEventsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let events = state
        .app_state
        .checkpoint_db
        .get_task_run_events(&id, query.event_type.as_deref(), query.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "events": events,
        "count": events.len()
    })))
}

/// Query parameters for paginated checkpoints.
#[derive(Debug, Deserialize)]
pub struct CheckpointsQuery {
    /// Cursor for pagination (step_index to start from, exclusive).
    cursor: Option<i64>,
    /// Maximum number of checkpoints to return (default: 50).
    limit: Option<usize>,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let limit = query.limit.unwrap_or(50).min(100); // Cap at 100 per page

    let (checkpoints, next_cursor) = state
        .app_state
        .checkpoint_db
        .get_workflow_step_checkpoints_paginated(&id, query.cursor, limit)
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

/// Query parameters for verification results.
#[derive(Debug, Deserialize)]
pub struct VerificationResultsQuery {
    /// Filter by iteration number (optional)
    iteration: Option<u32>,
    /// Only show failed results (optional, default: false)
    #[serde(default)]
    failed_only: bool,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get verification results
    let results = if let Some(iteration) = query.iteration {
        state
            .app_state
            .checkpoint_db
            .get_iteration_verification_results(&id, iteration)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        state
            .app_state
            .checkpoint_db
            .get_latest_verification_results(&id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    };

    // Filter by failed_only if requested
    let results: Vec<_> = if query.failed_only {
        results.into_iter().filter(|r| !r.passed).collect()
    } else {
        results
    };

    // Calculate summary stats
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let critical_failed = results
        .iter()
        .filter(|r| !r.passed && r.is_critical)
        .count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "results": results,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get all workflow verification phase results
    let results = state
        .app_state
        .checkpoint_db
        .get_all_verification_phase_results(&id)
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

/// Query parameters for MCP calls endpoint.
#[derive(Debug, Deserialize)]
pub struct McpCallsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get MCP calls
    let result = state
        .app_state
        .checkpoint_db
        .get_task_run_mcp_calls(&id, query.success)
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

/// Query parameters for API requests endpoint.
#[derive(Debug, Deserialize)]
pub struct ApiRequestsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get API requests
    let requests = state
        .app_state
        .checkpoint_db
        .get_task_run_api_requests(&id, query.success)
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

/// Query parameters for Playwright results endpoint.
#[derive(Debug, Deserialize)]
pub struct PlaywrightResultsQuery {
    /// Filter by status (optional: "passed", "failed", "skipped")
    status: Option<String>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get Playwright results
    let results = state
        .app_state
        .checkpoint_db
        .get_task_run_playwright_results(&id, query.status.as_deref())
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get AWAS steps
    let steps = state
        .app_state
        .checkpoint_db
        .get_task_run_awas_steps(&id)
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

/// Query parameters for knowledge endpoint.
#[derive(Debug, Deserialize)]
pub struct KnowledgeQuery {
    /// Filter by category (optional: "finding", "observation", "solution", etc.)
    category: Option<String>,
    /// Only show unresolved entries (optional, default: false)
    #[serde(default)]
    unresolved_only: bool,
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get knowledge entries
    let knowledge = state
        .app_state
        .checkpoint_db
        .list_task_knowledge(&id, query.category.as_deref(), query.unresolved_only)
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

/// Path parameters for step progress endpoint.
#[derive(Debug, Deserialize)]
pub struct StepProgressPath {
    id: String,
    checkpoint_id: String,
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
        .checkpoint_db
        .get_task_run(&path.id)
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
        .checkpoint_db
        .get_step_progress_markers(&path.checkpoint_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Also get the latest marker for quick access
    let latest = state
        .app_state
        .checkpoint_db
        .get_latest_step_progress_marker(&path.checkpoint_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "checkpoint_id": path.checkpoint_id,
        "markers": markers,
        "count": markers.len(),
        "latest": latest
    })))
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
        .checkpoint_db
        .get_running_task_runs()
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
    let events = state
        .app_state
        .checkpoint_db
        .get_task_run_events(&task.id, None, query.limit)
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
        .checkpoint_db
        .get_all_verification_phase_results(&task.id)
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
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let screenshots = state
        .app_state
        .checkpoint_db
        .get_task_run_screenshots(&id, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "screenshots": screenshots,
        "count": screenshots.len()
    })))
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

/// Get execution spans from SQLite (tracing data).
///
/// Supports filtering by execution_id, name pattern, and minimum duration.
pub async fn get_execution_spans(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ExecutionSpansQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let spans = state
        .app_state
        .checkpoint_db
        .get_execution_spans(
            query.execution_id.as_deref(),
            query.name_pattern.as_deref(),
            query.min_duration_ms,
            query.limit,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
        .checkpoint_db
        .get_task_run(&id)
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

    // Run migration
    let db = state.app_state.checkpoint_db.clone();
    let workflow_name = task_run.workflow_name.clone();
    let task_id = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        crate::log_migration::migrate_logs_to_sqlite(
            &db,
            &task_id,
            &dev_logs_dir,
            workflow_name.as_deref(),
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Task spawn error: {}", e),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "task_run_id": id,
        "migrated": {
            "general_events": result.general_events,
            "action_events": result.action_events,
            "image_recognition_events": result.image_recognition_events,
            "screenshots": result.screenshots,
            "playwright_results": result.playwright_results
        },
        "errors": result.errors
    })))
}

// ============================================================================
// Interactive AI Session Endpoints (HTTP equivalents of Tauri commands)
// ============================================================================

/// Request body for sending a user message to an active AI session.
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

    // Emit the user's message as an ai-output event so it appears in the conversation
    let session_ctx = None;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_ai_output(
            &state.app_handle,
            &req.message,
            "user_message",
            None,
            session_ctx,
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

// ============================================================================
// Batch Endpoint & Aggregation Helpers
// ============================================================================

/// Result of aggregating step events into execution data.
#[derive(Debug, Serialize)]
pub struct AggregatedStepData {
    pub steps: Vec<StepExecutionData>,
    pub has_setup: bool,
    pub has_verification: bool,
    pub has_agentic: bool,
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
    let db = state.app_state.checkpoint_db.clone();

    // Single blocking call for all DB work
    let batch = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let task_and_events = db.get_running_task_step_data()?;
        let (task, events) = match task_and_events {
            Some(pair) => pair,
            None => {
                return Ok(serde_json::json!({
                    "success": true,
                    "task_run_id": null,
                    "executions": [],
                    "completed_iterations": [],
                    "count": 0,
                    "message": "No running task"
                }));
            }
        };

        let completed_iterations = db.get_completed_verification_iterations(&task.id)?;
        let aggregated = aggregate_step_events(&events);

        Ok(serde_json::json!({
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
        }))
    })
    .await
    .map_err(|e| {
        error!("spawn_blocking error in get_current_execution_batch: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(batch))
}

// ============================================================================
// Remote Control / Mobile Chat Endpoints
// ============================================================================

/// SSE stream of AI output scoped to a specific task run.
///
/// Provides real-time AI conversation output for mobile/remote clients.
/// Includes initial catchup (current session state + recent output).
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

    // Spawn the Claude session
    match crate::claude_session::ClaudeSession::spawn(
        &working_dir,
        &task_run_id,
        &state.app_handle,
        None, // session_ctx
        None, // finding_ctx
        None, // progress_ctx
        None, // pid_tracker
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
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/task-runs", get(list_task_runs).post(create_task_run))
        .route("/task-runs/running", get(list_running_task_runs))
        .route("/task-runs/session", post(create_ai_session))
        .route("/task-runs/:id", get(get_task_run).delete(delete_task_run))
        .route("/task-runs/:id/output", get(get_task_output))
        .route("/task-runs/:id/workflow-state", get(get_workflow_state))
        .route("/task-runs/:id/result-data", get(get_task_run_result_data))
        .route("/task-runs/:id/orchestrator-state", get(get_workflow_state)) // Alias for backward compatibility
        .route("/task-runs/:id/full-state", get(get_full_workflow_state)) // Full state for restart recovery
        .route("/task-runs/:id/stop", post(stop_task_run))
        .route(
            "/task-runs/:id/auto-continue",
            get(get_task_auto_continue).put(set_task_auto_continue),
        )
        .route("/task-runs/:id/resume", post(resume_task_run))
        .route(
            "/task-runs/:id/generate-summary",
            post(generate_task_summary),
        )
        .route("/task-runs/:id/events", get(get_task_run_events))
        .route("/task-runs/:id/screenshots", get(get_task_run_screenshots))
        .route(
            "/task-runs/:id/playwright-results",
            get(get_task_run_playwright_results),
        )
        .route("/execution-spans", get(get_execution_spans))
        .route("/task-runs/:id/migrate-logs", post(migrate_task_run_logs))
        .route("/task-runs/:id/checkpoints", get(get_task_run_checkpoints))
        .route(
            "/task-runs/:id/verification-results",
            get(get_task_run_verification_results),
        )
        .route(
            "/task-runs/:id/verification-phase-results",
            get(get_task_run_verification_phase_results),
        )
        .route("/task-runs/:id/mcp-calls", get(get_task_run_mcp_calls))
        .route(
            "/task-runs/:id/api-requests",
            get(get_task_run_api_requests),
        )
        .route("/task-runs/:id/awas-steps", get(get_task_run_awas_steps))
        .route("/task-runs/:id/knowledge", get(get_task_run_knowledge))
        .route(
            "/task-runs/:id/steps/:checkpoint_id/progress",
            get(get_step_progress_markers),
        )
        .route("/task-runs/:id/message", post(send_message_to_session))
        .route("/task-runs/:id/session-state", get(get_session_state))
        .route(
            "/task-runs/:id/generate-workflow",
            post(generate_workflow_from_session),
        )
        .route("/task-runs/:id/rename", post(rename_task_run))
        .route(
            "/task-runs/:id/stream/ai-output",
            get(sse_ai_output_for_task_run),
        )
        .route("/task-runs/:id/approvals", get(list_approvals))
        .route("/task-runs/:id/approvals/:aid", get(get_approval))
        .route(
            "/task-runs/:id/approvals/:aid/respond",
            post(respond_to_approval),
        )
        .route("/task-runs/:id/approval-gates", get(get_approval_gates))
        .route("/current-execution/steps", get(get_current_execution_steps))
        .route("/current-execution/batch", get(get_current_execution_batch))
        .route("/task-runs/:id/usage", get(get_task_run_usage))
        .route("/traces/:trace_id", get(get_trace_correlation))
}

/// Get per-phase token usage breakdown for a task run.
async fn get_task_run_usage(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();
    let task_run_id = id.clone();

    let usage = tokio::task::spawn_blocking(move || db.get_phase_token_usage(&task_run_id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn_blocking error: {}", e),
            )
        })?
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
async fn get_trace_correlation(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(trace_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();
    let tid = trace_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db.get_conn_string()?;

        // Query execution spans for this trace
        let mut span_stmt = conn
            .prepare(
                r#"
                SELECT id, execution_id, trace_id, span_id, parent_span_id, name,
                       start_ts, end_ts, duration_ms, attributes, success, error, created_at
                FROM execution_spans
                WHERE trace_id = ?1
                ORDER BY start_ts ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare spans query: {}", e))?;

        let spans: Vec<serde_json::Value> = span_stmt
            .query_map(rusqlite::params![&tid], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "execution_id": row.get::<_, Option<String>>(1)?,
                    "trace_id": row.get::<_, String>(2)?,
                    "span_id": row.get::<_, String>(3)?,
                    "parent_span_id": row.get::<_, Option<String>>(4)?,
                    "name": row.get::<_, String>(5)?,
                    "start_ts": row.get::<_, String>(6)?,
                    "end_ts": row.get::<_, Option<String>>(7)?,
                    "duration_ms": row.get::<_, Option<i64>>(8)?,
                    "attributes": row.get::<_, Option<String>>(9)?,
                    "success": row.get::<_, Option<i32>>(10)?,
                    "error": row.get::<_, Option<String>>(11)?,
                    "created_at": row.get::<_, String>(12)?,
                }))
            })
            .map_err(|e| format!("Failed to query spans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Query error events for this trace
        let mut error_stmt = conn
            .prepare(
                r#"
                SELECT id, log_source_name, task_run_id, severity, error_type,
                       message, stack_trace, captured_at, status, trace_id
                FROM error_events
                WHERE trace_id = ?1
                ORDER BY captured_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare errors query: {}", e))?;

        let errors: Vec<serde_json::Value> = error_stmt
            .query_map(rusqlite::params![&tid], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "log_source_name": row.get::<_, String>(1)?,
                    "task_run_id": row.get::<_, Option<String>>(2)?,
                    "severity": row.get::<_, String>(3)?,
                    "error_type": row.get::<_, Option<String>>(4)?,
                    "message": row.get::<_, String>(5)?,
                    "stack_trace": row.get::<_, Option<String>>(6)?,
                    "captured_at": row.get::<_, String>(7)?,
                    "status": row.get::<_, String>(8)?,
                    "trace_id": row.get::<_, Option<String>>(9)?,
                }))
            })
            .map_err(|e| format!("Failed to query errors: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok::<_, String>(serde_json::json!({
            "trace_id": tid,
            "execution_spans": spans,
            "error_events": errors,
            "span_count": spans.len(),
            "error_count": errors.len(),
        }))
    })
    .await
    .map_err(|e| {
        error!("spawn_blocking error in get_trace_correlation: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::TaskRunEvent;

    fn make_event(
        id: i64,
        event_type: &str,
        event_subtype: &str,
        action_id: Option<&str>,
        data_json: Option<serde_json::Value>,
        timestamp: &str,
        duration_ms: Option<i64>,
    ) -> TaskRunEvent {
        TaskRunEvent {
            id,
            task_run_id: "test-run".to_string(),
            event_type: event_type.to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: "test step".to_string(),
            data: data_json.map(|v| v.to_string()),
            workflow_name: None,
            state_name: None,
            action_id: action_id.map(|s| s.to_string()),
            timestamp: timestamp.to_string(),
            duration_ms,
        }
    }

    #[test]
    fn test_aggregate_step_events_empty() {
        let events: Vec<TaskRunEvent> = vec![];
        let result = aggregate_step_events(&events);

        assert!(result.steps.is_empty());
        assert!(!result.has_setup);
        assert!(!result.has_verification);
        assert!(!result.has_agentic);
    }

    #[test]
    fn test_aggregate_step_events_basic() {
        let events = vec![
            make_event(
                1,
                "step_execution",
                "start",
                Some("action-1"),
                Some(serde_json::json!({
                    "step_name": "run tests",
                    "step_index": 0,
                    "step_type": "shell_command",
                    "phase": "setup"
                })),
                "2026-01-01T00:00:00Z",
                None,
            ),
            make_event(
                2,
                "step_execution",
                "complete",
                Some("action-1"),
                Some(serde_json::json!({
                    "step_name": "run tests",
                    "step_index": 0,
                    "step_type": "shell_command",
                    "phase": "setup",
                    "duration_ms": 2500,
                    "exit_code": 0,
                    "stdout": "all tests passed"
                })),
                "2026-01-01T00:00:03Z",
                Some(2500),
            ),
        ];

        let result = aggregate_step_events(&events);

        assert_eq!(result.steps.len(), 1);
        let step = &result.steps[0];
        assert_eq!(step.step_name, "run tests");
        assert_eq!(step.step_type, "shell_command");
        assert_eq!(step.status, "success");
        assert_eq!(step.phase.as_deref(), Some("setup"));
        assert_eq!(step.step_index, Some(0));
        assert_eq!(step.duration_ms, Some(2500));
        assert_eq!(step.exit_code, Some(0));
        assert_eq!(step.stdout.as_deref(), Some("all tests passed"));
    }

    #[test]
    fn test_aggregate_step_events_phase_tracking() {
        let events = vec![
            // Setup step
            make_event(
                1,
                "step_execution",
                "start",
                Some("s-1"),
                Some(serde_json::json!({
                    "step_name": "install deps",
                    "step_index": 0,
                    "phase": "setup"
                })),
                "2026-01-01T00:00:00Z",
                None,
            ),
            // Verification step
            make_event(
                2,
                "step_execution",
                "start",
                Some("v-1"),
                Some(serde_json::json!({
                    "step_name": "check output",
                    "step_index": 0,
                    "phase": "verification",
                    "iteration": 1
                })),
                "2026-01-01T00:01:00Z",
                None,
            ),
            // Agentic step
            make_event(
                3,
                "step_execution",
                "start",
                Some("a-1"),
                Some(serde_json::json!({
                    "step_name": "fix issues",
                    "step_index": 0,
                    "phase": "agentic",
                    "iteration": 1
                })),
                "2026-01-01T00:02:00Z",
                None,
            ),
        ];

        let result = aggregate_step_events(&events);

        assert_eq!(result.steps.len(), 3);
        assert!(result.has_setup);
        assert!(result.has_verification);
        assert!(result.has_agentic);
    }

    #[test]
    fn test_aggregate_step_events_ignores_non_step_events() {
        let events = vec![
            // ai_output event should be ignored
            make_event(
                1,
                "ai_output",
                "text",
                None,
                Some(serde_json::json!({"text": "thinking..."})),
                "2026-01-01T00:00:00Z",
                None,
            ),
            // state_change event should be ignored
            make_event(
                2,
                "state_change",
                "transition",
                None,
                Some(serde_json::json!({"from": "idle", "to": "running"})),
                "2026-01-01T00:00:01Z",
                None,
            ),
            // Only this step_execution should be counted
            make_event(
                3,
                "step_execution",
                "start",
                Some("act-1"),
                Some(serde_json::json!({
                    "step_name": "build",
                    "step_index": 0,
                    "phase": "setup"
                })),
                "2026-01-01T00:00:02Z",
                None,
            ),
        ];

        let result = aggregate_step_events(&events);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].step_name, "build");
    }

    #[test]
    fn test_aggregate_step_events_failed_status_is_sticky() {
        // If a step gets both a "failed" and "complete" event, it should stay "failed"
        let events = vec![
            make_event(
                1,
                "step_execution",
                "start",
                Some("action-x"),
                Some(serde_json::json!({
                    "step_name": "flaky step",
                    "step_index": 0,
                    "phase": "verification",
                    "iteration": 1
                })),
                "2026-01-01T00:00:00Z",
                None,
            ),
            make_event(
                2,
                "step_execution",
                "failed",
                Some("action-x"),
                Some(serde_json::json!({
                    "step_name": "flaky step",
                    "step_index": 0,
                    "error": "assertion failed"
                })),
                "2026-01-01T00:00:01Z",
                None,
            ),
            make_event(
                3,
                "step_execution",
                "complete",
                Some("action-x"),
                Some(serde_json::json!({
                    "step_name": "flaky step",
                    "step_index": 0,
                    "duration_ms": 500
                })),
                "2026-01-01T00:00:02Z",
                Some(500),
            ),
        ];

        let result = aggregate_step_events(&events);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].status, "failed");
        assert_eq!(result.steps[0].error.as_deref(), Some("assertion failed"));
    }
}
