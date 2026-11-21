//! Workflow state and resume endpoints extracted from task_runs.rs.
//!
//! Contains handlers for:
//! - Workflow state queries (get_workflow_state)
//! - Result data (get_task_run_result_data)
//! - Resume/restart (resume_task_run)
//!
//! The generic task-run reads (`/task-runs/{id}/output`, `/full-state`) live
//! in `task_run_full_state`.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use super::task_run_full_state::{pg_get_task_run, state_name_to_stage};
use crate::database::TaskRun;
use crate::mcp::types::ApiState;
use crate::unified_workflows::UnifiedWorkflowExt;
use tauri::Manager;

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
    use crate::step_executor::extract_workflow_id_from_task_id;
    use crate::unified_workflow_executor::{LoopConfig, LoopController};

    info!("Resume task run request: {}", id);

    // Coord's device drain (plan `2026-09-13-drained-runner-never-reaches-idle`):
    // an HTTP-shaped caller is autonomous (`unknown`), refused before the row is
    // reopened. The routed door is `mcp/task_runs.rs`'s; this twin is gated the
    // same way so no path to a workflow spawn stays ungated.
    if let crate::coord_drain_state::DrainGate::Defer { reason, class } =
        crate::coord_drain_state::drain_gate_for_work(
            crate::coord_drain_state::SpawnOrigin::Unknown,
            &format!("resume_task_run:{id}"),
        )
    {
        return Err((StatusCode::CONFLICT, format!("{}: {reason}", class.code())));
    }

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
    let stages: Vec<crate::step_executor::StageConfig> = normalized_stages
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
        auto_commit_subagents: None,
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
