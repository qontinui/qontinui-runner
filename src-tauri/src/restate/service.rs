//! Restate SDK service definitions.
//!
//! Defines the QontinuiWorkflow and WorkflowStateObject services
//! that the Restate server calls into for durable workflow execution.
//!
//! These services use Restate's proc macros to generate the HTTP handler
//! bindings and client code automatically.
//!
//! This entire module is gated behind `#[cfg(feature = "restate")]` because
//! it depends on the `restate-sdk` crate's proc macros.

use std::sync::Arc;

use restate_sdk::prelude::*;
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::AppState;

use super::compensation::CompensationAction;
use super::types::{
    ApprovalResponse, DurablePhase, DurableWorkflowState, WorkflowInput, WorkflowOutput,
};

// ---------------------------------------------------------------------------
// State keys — these are the Restate key names used for get()/set() calls.
// ---------------------------------------------------------------------------

const STATE_KEY: &str = "workflow_state";
const COMPENSATIONS_KEY: &str = "compensations";

// ---------------------------------------------------------------------------
// Global state for Restate service handlers.
// The Restate SDK requires stateless service impls, so we store shared
// references globally and initialize them before starting the endpoint.
// ---------------------------------------------------------------------------

static APP_STATE: std::sync::OnceLock<Arc<AppState>> = std::sync::OnceLock::new();
static CONFIG_STORAGE: std::sync::OnceLock<Arc<tokio::sync::Mutex<ConfigStorage>>> =
    std::sync::OnceLock::new();

/// Initialize the global state for Restate service handlers.
/// Must be called before starting the Restate HTTP endpoint.
pub fn init_global_state(
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
) {
    let _ = APP_STATE.set(app_state);
    let _ = CONFIG_STORAGE.set(config_storage);
}

fn get_app_state() -> Result<&'static Arc<AppState>, TerminalError> {
    APP_STATE
        .get()
        .ok_or_else(|| TerminalError::new("Restate global AppState not initialized"))
}

fn get_config_storage() -> Result<&'static Arc<tokio::sync::Mutex<ConfigStorage>>, TerminalError> {
    CONFIG_STORAGE
        .get()
        .ok_or_else(|| TerminalError::new("Restate global ConfigStorage not initialized"))
}

// ---------------------------------------------------------------------------
// QontinuiWorkflow — Restate Workflow (runs exactly once per execution_id)
// ---------------------------------------------------------------------------

#[restate_sdk::workflow]
pub trait QontinuiWorkflow {
    /// Main execution handler. Runs the full 4-phase workflow loop:
    /// Setup → Verification → Agentic → Completion.
    ///
    /// The `execution_id` serves as the Restate workflow key, guaranteeing
    /// exactly-once semantics per workflow run.
    async fn run(input: WorkflowInput) -> Result<WorkflowOutput, TerminalError>;

    /// Shared handler to query current workflow state.
    /// Can be called concurrently while `run()` is executing.
    #[shared]
    async fn get_state() -> Result<DurableWorkflowState, TerminalError>;

    /// Shared handler to signal a graceful stop request.
    /// Sets the `stop_requested` flag so the next phase boundary checks it.
    #[shared]
    async fn request_stop() -> Result<(), TerminalError>;

    /// Shared handler for approval gate resolution.
    /// Resolves the awakeable that the run() handler is waiting on.
    #[shared]
    async fn submit_approval(response: ApprovalResponse) -> Result<(), TerminalError>;
}

pub struct QontinuiWorkflowImpl;

impl QontinuiWorkflow for QontinuiWorkflowImpl {
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        input: WorkflowInput,
    ) -> Result<WorkflowOutput, TerminalError> {
        let execution_id = ctx.key().to_string();
        info!(
            execution_id = %execution_id,
            "Starting durable workflow execution"
        );

        // Store initial state
        let initial_state = DurableWorkflowState {
            execution_id: execution_id.clone(),
            workflow_name: String::from("workflow"),
            phase: DurablePhase::Setup,
            iteration: 0,
            stage_index: None,
            verification_passed: false,
            stop_requested: false,
            approval_awakeable_id: None,
            total_steps_completed: 0,
            last_completed_step: None,
        };

        ctx.set(STATE_KEY, initial_state);

        // Initialize empty compensation stack (wrapped in `Json<…>` because
        // `Vec<T>` has no direct `restate_sdk::serde` impl for generic `T`).
        let compensations: Vec<CompensationAction> = Vec::new();
        ctx.set(COMPENSATIONS_KEY, restate_sdk::serde::Json(compensations));

        // Get global state references
        let app_state = get_app_state()?;
        let config_storage = get_config_storage()?;
        let pg_db = &app_state.pg_db;

        // Parse max_iterations from config
        let max_iterations: u32 = serde_json::from_str::<serde_json::Value>(&input.loop_config_json)
            .ok()
            .and_then(|v| v.get("max_iterations")?.as_u64())
            .unwrap_or(5) as u32;

        let start_time = std::time::Instant::now();
        let mut verification_passed = false;
        let mut was_stopped = false;
        let mut iterations_run = 0u32;

        // Update task run status
        if let Err(e) = pg_db.update_task_run_status(&execution_id, "running").await {
            warn!(execution_id = %execution_id, "Failed to update task run status: {}", e);
        }

        // =====================================================================
        // PHASE 1: SETUP (each phase is a separate ctx.run() for journal replay)
        // =====================================================================
        info!(execution_id = %execution_id, "Phase 1/4: Setup");
        ctx.set(
            STATE_KEY,
            DurableWorkflowState {
                execution_id: execution_id.clone(),
                workflow_name: String::from("workflow"),
                phase: DurablePhase::Setup,
                ..Default::default()
            },
        );

        // Setup automation steps — journaled as a single side effect
        let setup_result: super::durable_executor::PhaseResult = {
            let as_clone = app_state.clone();
            let cs_clone = config_storage.clone();
            let steps = input.setup_automation_steps_json.clone();
            let eid = execution_id.clone();
            ctx.run(|| async move {
                Ok(super::durable_executor::execute_steps_batch(
                    &as_clone, &cs_clone, &steps, "setup", None, &eid, None,
                )
                .await)
            })
            .name("phase-setup-auto")
            .await
            .map_err(|e| TerminalError::new(format!("Setup failed: {}", e)))?
        };

        if !setup_result.success {
            let output = WorkflowOutput {
                success: false,
                verification_passed: false,
                iterations_run: 0,
                critical_failure: true,
                was_stopped: false,
                duration_ms: start_time.elapsed().as_millis() as u64,
                files_modified: vec![],
                error: setup_result.failure_context,
            };
            if let Err(e) = pg_db.update_task_run_status(&execution_id, "failed").await {
                warn!(execution_id = %execution_id, "Failed to update status: {}", e);
            }
            return Ok(output);
        }

        // Setup prompt steps — journaled separately
        let _setup_prompt_result: super::durable_executor::PhaseResult = {
            let as_clone = app_state.clone();
            let cs_clone = config_storage.clone();
            let steps = input.setup_prompt_steps_json.clone();
            let eid = execution_id.clone();
            ctx.run(|| async move {
                Ok(super::durable_executor::execute_steps_batch(
                    &as_clone,
                    &cs_clone,
                    &steps,
                    "setup_prompt",
                    None,
                    &eid,
                    None,
                )
                .await)
            })
            .name("phase-setup-prompt")
            .await
            .map_err(|e| TerminalError::new(format!("Setup prompts failed: {}", e)))?
        };

        // =====================================================================
        // PHASE 2-3: VERIFICATION-AGENTIC LOOP (each iteration journaled)
        // =====================================================================
        let mut iteration = 1u32;
        while iteration <= max_iterations {
            // Check stop via PG (not journaled — side-effect-free read)
            let stopped = {
                match pg_db.get_task_run(&execution_id).await {
                    Ok(Some(tr)) => tr.status == "stopped" || tr.status == "cancelling",
                    _ => false,
                }
            };
            if stopped {
                info!(execution_id = %execution_id, iteration, "Stop requested");
                was_stopped = true;
                break;
            }

            // Update state for this iteration
            ctx.set(
                STATE_KEY,
                DurableWorkflowState {
                    execution_id: execution_id.clone(),
                    workflow_name: String::from("workflow"),
                    phase: DurablePhase::Verification,
                    iteration,
                    ..Default::default()
                },
            );

            // Verification — journaled per iteration
            let v_result: super::durable_executor::PhaseResult = {
                let as_clone = app_state.clone();
                let cs_clone = config_storage.clone();
                let steps = input.verification_steps_json.clone();
                let eid = execution_id.clone();
                let iter = iteration;
                ctx.run(|| async move {
                    Ok(super::durable_executor::execute_steps_batch(
                        &as_clone,
                        &cs_clone,
                        &steps,
                        "verification",
                        Some(iter),
                        &eid,
                        None,
                    )
                    .await)
                })
                .name(format!("phase-verify-{}", iteration))
                .await
                .map_err(|e| TerminalError::new(format!("Verification failed: {}", e)))?
            };

            iterations_run = iteration;

            if v_result.all_passed {
                info!(execution_id = %execution_id, iteration, "Verification PASSED");
                verification_passed = true;
                break;
            }

            let failure_ctx = v_result
                .failure_context
                .unwrap_or_else(|| "Verification failed".to_string());

            // Agentic — journaled per iteration
            if iteration < max_iterations {
                ctx.set(
                    STATE_KEY,
                    DurableWorkflowState {
                        execution_id: execution_id.clone(),
                        workflow_name: String::from("workflow"),
                        phase: DurablePhase::Agentic,
                        iteration,
                        ..Default::default()
                    },
                );

                let _a_result: super::durable_executor::PhaseResult = {
                    let as_clone = app_state.clone();
                    let cs_clone = config_storage.clone();
                    let steps = input.agentic_steps_json.clone();
                    let eid = execution_id.clone();
                    let iter = iteration;
                    let fail_ctx = failure_ctx.clone();
                    ctx.run(|| async move {
                        Ok(super::durable_executor::execute_steps_batch(
                            &as_clone,
                            &cs_clone,
                            &steps,
                            "agentic",
                            Some(iter),
                            &eid,
                            Some(&fail_ctx),
                        )
                        .await)
                    })
                    .name(format!("phase-agentic-{}", iteration))
                    .await
                    .map_err(|e| TerminalError::new(format!("Agentic failed: {}", e)))?
                };
            }

            iteration += 1;
        }

        // =====================================================================
        // PHASE 4: COMPLETION (journaled separately)
        // =====================================================================
        if verification_passed {
            info!(execution_id = %execution_id, "Phase 4/4: Completion");
            ctx.set(
                STATE_KEY,
                DurableWorkflowState {
                    execution_id: execution_id.clone(),
                    workflow_name: String::from("workflow"),
                    phase: DurablePhase::Completion,
                    verification_passed: true,
                    ..Default::default()
                },
            );

            // Completion automation
            let _c_result: super::durable_executor::PhaseResult = {
                let as_clone = app_state.clone();
                let cs_clone = config_storage.clone();
                let steps = input.completion_automation_steps_json.clone();
                let eid = execution_id.clone();
                ctx.run(|| async move {
                    Ok(super::durable_executor::execute_steps_batch(
                        &as_clone,
                        &cs_clone,
                        &steps,
                        "completion",
                        None,
                        &eid,
                        None,
                    )
                    .await)
                })
                .name("phase-completion-auto")
                .await
                .map_err(|e| TerminalError::new(format!("Completion failed: {}", e)))?
            };

            // Completion prompts
            let _cp_result: super::durable_executor::PhaseResult = {
                let as_clone = app_state.clone();
                let cs_clone = config_storage.clone();
                let steps = input.completion_prompt_steps_json.clone();
                let eid = execution_id.clone();
                ctx.run(|| async move {
                    Ok(super::durable_executor::execute_steps_batch(
                        &as_clone,
                        &cs_clone,
                        &steps,
                        "completion_prompt",
                        None,
                        &eid,
                        None,
                    )
                    .await)
                })
                .name("phase-completion-prompt")
                .await
                .map_err(|e| TerminalError::new(format!("Completion prompts failed: {}", e)))?
            };
        }

        // =====================================================================
        // FINALIZE
        // =====================================================================
        let duration_ms = start_time.elapsed().as_millis() as u64;
        let success = verification_passed && !was_stopped;

        // Collect modified files via git
        let files_modified: Vec<String> = {
            let _as_clone = app_state.clone();
            // `Vec<String>` lacks a direct `restate_sdk::serde` impl, so wrap
            // it in `Json<…>` for the journal payload and unwrap after.
            ctx.run(|| async move {
                Ok(restate_sdk::serde::Json(
                    super::durable_executor::get_modified_files().await,
                ))
            })
            .name("collect-modified-files")
            .await
            .map(|j: restate_sdk::serde::Json<Vec<String>>| j.into_inner())
            .unwrap_or_default()
        };

        let final_status = if success {
            "complete"
        } else if was_stopped {
            "stopped"
        } else {
            "failed"
        };
        if let Err(e) = pg_db
            .update_task_run_status(&execution_id, final_status)
            .await
        {
            warn!(execution_id = %execution_id, "Failed to update final status: {}", e);
        }

        // Update final Restate state
        let final_phase = if was_stopped {
            DurablePhase::Stopped
        } else if success {
            DurablePhase::Completed
        } else {
            DurablePhase::Failed
        };

        ctx.set(
            STATE_KEY,
            DurableWorkflowState {
                execution_id: execution_id.clone(),
                workflow_name: String::from("workflow"),
                phase: final_phase,
                iteration: iterations_run,
                verification_passed,
                stop_requested: was_stopped,
                ..Default::default()
            },
        );

        // If failed, execute saga compensations
        if !success && !was_stopped {
            let compensations: Option<restate_sdk::serde::Json<Vec<CompensationAction>>> = ctx
                .get(COMPENSATIONS_KEY)
                .await
                .map_err(|e| TerminalError::new(format!("Failed to read compensations: {}", e)))?;

            if let Some(actions) = compensations.map(|j| j.into_inner()) {
                if !actions.is_empty() {
                    info!(execution_id = %execution_id, count = actions.len(),
                        "Executing saga compensations for failed workflow");
                    let results = super::compensation::execute_all_compensations(&actions).await;
                    let failed = results.iter().filter(|r| !r.success).count();
                    if failed > 0 {
                        warn!(execution_id = %execution_id, failed, "Some compensations failed");
                    }
                }
            }
        }

        let output = WorkflowOutput {
            success,
            verification_passed,
            iterations_run,
            critical_failure: false,
            was_stopped,
            duration_ms,
            files_modified,
            error: None,
        };

        info!(
            execution_id = %execution_id,
            success = output.success,
            verification_passed = output.verification_passed,
            iterations = output.iterations_run,
            duration_ms = output.duration_ms,
            "Durable workflow execution completed"
        );

        Ok(output)
    }

    async fn get_state(
        &self,
        ctx: SharedWorkflowContext<'_>,
    ) -> Result<DurableWorkflowState, TerminalError> {
        let state: Option<DurableWorkflowState> = ctx
            .get(STATE_KEY)
            .await
            .map_err(|e| TerminalError::new(format!("Failed to read state: {}", e)))?;

        Ok(state.unwrap_or_default())
    }

    async fn request_stop(&self, ctx: SharedWorkflowContext<'_>) -> Result<(), TerminalError> {
        let execution_id = ctx.key().to_string();
        warn!(
            execution_id = %execution_id,
            "Stop requested for durable workflow"
        );

        // SharedWorkflowContext is read-only — cannot call set().
        // Instead, signal the stop through the database. The running workflow's
        // is_stop_requested() polls PG for status == "cancelling" || "stopped".
        let app_state = get_app_state()?;
        if let Err(e) = app_state
            .pg_db
            .update_task_run_status(&execution_id, "cancelling")
            .await
        {
            error!(
                execution_id = %execution_id,
                "Failed to set cancelling status in PG: {}", e
            );
            return Err(TerminalError::new(format!("Failed to request stop: {}", e)));
        }

        info!(
            execution_id = %execution_id,
            "Stop signal stored in PG (status=cancelling)"
        );
        Ok(())
    }

    async fn submit_approval(
        &self,
        ctx: SharedWorkflowContext<'_>,
        response: ApprovalResponse,
    ) -> Result<(), TerminalError> {
        let execution_id = ctx.key().to_string();
        info!(
            execution_id = %execution_id,
            approved = response.approved,
            "Approval response submitted for durable workflow"
        );

        // SharedWorkflowContext is read-only — cannot resolve awakeables directly.
        // Instead, resolve the awakeable via the Restate ingress API.
        // The awakeable ID is stored in PG (restate_awakeables table).
        let app_state = get_app_state()?;
        let pending = app_state
            .pg_db
            .get_pending_awakeables(&execution_id)
            .await
            .map_err(|e| TerminalError::new(format!("Failed to get pending awakeables: {}", e)))?;

        if let Some(awakeable) = pending.first() {
            let restate_settings = crate::settings::load_settings().restate;
            let payload = serde_json::json!({
                "approved": response.approved,
                "comment": response.comment,
                "reviewer": response.reviewer,
            });

            if let Err(e) = super::launch::resolve_awakeable(
                &awakeable.awakeable_id,
                &payload,
                &restate_settings.ingress_url(),
            )
            .await
            {
                error!(
                    execution_id = %execution_id,
                    "Failed to resolve awakeable: {}", e
                );
                return Err(TerminalError::new(format!(
                    "Failed to resolve awakeable: {}",
                    e
                )));
            }

            // Mark as resolved in PG
            if let Err(e) = app_state
                .pg_db
                .resolve_restate_awakeable(&awakeable.awakeable_id)
                .await
            {
                warn!(
                    execution_id = %execution_id,
                    "Failed to update awakeable status in PG: {}", e
                );
            }

            info!(
                execution_id = %execution_id,
                awakeable_id = %awakeable.awakeable_id,
                "Approval awakeable resolved"
            );
        } else {
            warn!(
                execution_id = %execution_id,
                "No pending awakeables found for approval"
            );
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WorkflowStateObject — Restate Virtual Object (per-execution mutable state)
// ---------------------------------------------------------------------------

#[restate_sdk::object]
pub trait WorkflowStateObject {
    /// Read current workflow state.
    #[shared]
    async fn get_state() -> Result<DurableWorkflowState, TerminalError>;

    /// Update workflow state. Called from the workflow's `run()` handler
    /// at phase boundaries.
    async fn set_state(state: DurableWorkflowState) -> Result<(), TerminalError>;

    /// Record a compensation action onto the LIFO stack.
    async fn push_compensation(action: CompensationAction) -> Result<(), TerminalError>;

    /// Retrieve the full compensation stack (in recording order;
    /// caller is responsible for reversing for LIFO execution).
    ///
    /// Returned as `Json<Vec<CompensationAction>>` because `Vec<T>` has no
    /// direct `restate_sdk::serde` impl for generic `T`. Callers should call
    /// `.into_inner()` to access the vec.
    #[shared]
    async fn get_compensations(
    ) -> Result<restate_sdk::serde::Json<Vec<CompensationAction>>, TerminalError>;
}

pub struct WorkflowStateObjectImpl;

impl WorkflowStateObject for WorkflowStateObjectImpl {
    async fn get_state(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<DurableWorkflowState, TerminalError> {
        let state: Option<DurableWorkflowState> = ctx
            .get(STATE_KEY)
            .await
            .map_err(|e| TerminalError::new(format!("Failed to read state: {}", e)))?;

        Ok(state.unwrap_or_default())
    }

    async fn set_state(
        &self,
        ctx: ObjectContext<'_>,
        state: DurableWorkflowState,
    ) -> Result<(), TerminalError> {
        let execution_id = ctx.key().to_string();
        info!(
            execution_id = %execution_id,
            phase = %state.phase.as_str(),
            iteration = state.iteration,
            "Updating durable workflow state"
        );

        ctx.set(STATE_KEY, state);
        Ok(())
    }

    async fn push_compensation(
        &self,
        ctx: ObjectContext<'_>,
        action: CompensationAction,
    ) -> Result<(), TerminalError> {
        let execution_id = ctx.key().to_string();
        info!(
            execution_id = %execution_id,
            action_id = %action.id,
            description = %action.description,
            "Recording compensation action"
        );

        let mut compensations: Vec<CompensationAction> = ctx
            .get::<restate_sdk::serde::Json<Vec<CompensationAction>>>(COMPENSATIONS_KEY)
            .await
            .map_err(|e| TerminalError::new(format!("Failed to read compensations: {}", e)))?
            .map(|j| j.into_inner())
            .unwrap_or_default();

        compensations.push(action);
        ctx.set(COMPENSATIONS_KEY, restate_sdk::serde::Json(compensations));

        Ok(())
    }

    async fn get_compensations(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<restate_sdk::serde::Json<Vec<CompensationAction>>, TerminalError> {
        let compensations: Vec<CompensationAction> = ctx
            .get::<restate_sdk::serde::Json<Vec<CompensationAction>>>(COMPENSATIONS_KEY)
            .await
            .map_err(|e| TerminalError::new(format!("Failed to read compensations: {}", e)))?
            .map(|j| j.into_inner())
            .unwrap_or_default();

        Ok(restate_sdk::serde::Json(compensations))
    }
}
