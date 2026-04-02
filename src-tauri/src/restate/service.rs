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

#![cfg(feature = "restate")]

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

        // Initialize empty compensation stack
        let compensations: Vec<CompensationAction> = Vec::new();
        ctx.set(COMPENSATIONS_KEY, compensations);

        // Get global state references
        let app_state = get_app_state()?;
        let config_storage = get_config_storage()?;
        let pg_db = &app_state.pg_db;

        // Execute the full workflow via the durable executor.
        // The entire workflow runs as a single Restate side effect (ctx.run).
        // On crash + replay, Restate returns the cached output without re-executing.
        let input_clone = input.clone();
        let execution_id_clone = execution_id.clone();
        let app_state_clone = app_state.clone();
        let config_storage_clone = config_storage.clone();
        let pg_db_clone = pg_db.clone();

        let output: WorkflowOutput = ctx
            .run(|| async move {
                super::durable_executor::execute_workflow(
                    &app_state_clone,
                    &config_storage_clone,
                    &input_clone,
                    &execution_id_clone,
                    &pg_db_clone,
                )
                .await
            })
            .name("workflow-execution")
            .await
            .map_err(|e| TerminalError::new(format!("Workflow execution failed: {}", e)))?;

        // Update final state in Restate K/V
        let final_phase = if output.was_stopped {
            DurablePhase::Stopped
        } else if output.success {
            DurablePhase::Completed
        } else {
            DurablePhase::Failed
        };

        let final_state = DurableWorkflowState {
            execution_id: execution_id.clone(),
            workflow_name: String::from("workflow"),
            phase: final_phase,
            iteration: output.iterations_run,
            stage_index: None,
            verification_passed: output.verification_passed,
            stop_requested: output.was_stopped,
            approval_awakeable_id: None,
            total_steps_completed: 0,
            last_completed_step: None,
        };
        ctx.set(STATE_KEY, final_state);

        // If the workflow failed and compensations exist, execute them
        if !output.success && !output.was_stopped {
            let compensations: Option<Vec<CompensationAction>> =
                ctx.get(COMPENSATIONS_KEY).await.map_err(|e| {
                    TerminalError::new(format!("Failed to read compensations: {}", e))
                })?;

            if let Some(actions) = compensations {
                if !actions.is_empty() {
                    info!(
                        execution_id = %execution_id,
                        count = actions.len(),
                        "Executing saga compensations for failed workflow"
                    );
                    let results =
                        super::compensation::execute_all_compensations(&actions).await;
                    let failed = results.iter().filter(|r| !r.success).count();
                    if failed > 0 {
                        warn!(
                            execution_id = %execution_id,
                            failed = failed,
                            "Some compensations failed"
                        );
                    }
                }
            }
        }

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
        let state: Option<DurableWorkflowState> = ctx.get(STATE_KEY).await.map_err(|e| {
            TerminalError::new(format!("Failed to read state: {}", e))
        })?;

        Ok(state.unwrap_or_default())
    }

    async fn request_stop(
        &self,
        ctx: SharedWorkflowContext<'_>,
    ) -> Result<(), TerminalError> {
        let execution_id = ctx.key().to_string();
        warn!(
            execution_id = %execution_id,
            "Stop requested for durable workflow"
        );

        // NOTE: SharedWorkflowContext is read-only — it cannot call set().
        // The stop signal must be communicated through a different mechanism.
        //
        // TODO(Phase 3): Use a Restate awakeable or signal to communicate
        // the stop request to the running workflow. Possible approaches:
        // 1. Use ctx.run_client() to call a Virtual Object that stores the flag
        // 2. Use an external signal channel (e.g., tokio watch channel)
        // 3. Store the flag in the WorkflowStateObject and poll from run()

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

        // NOTE: SharedWorkflowContext is read-only — it cannot resolve awakeables.
        //
        // TODO(Phase 3): Resolve the approval awakeable. The awakeable ID is stored
        // in the workflow state's `approval_awakeable_id` field. The resolution
        // needs to happen through:
        // 1. A Restate admin API call to resolve the awakeable externally
        // 2. Or routing through the WorkflowStateObject to store the response
        //    and having run() poll for it

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
    #[shared]
    async fn get_compensations() -> Result<Vec<CompensationAction>, TerminalError>;
}

pub struct WorkflowStateObjectImpl;

impl WorkflowStateObject for WorkflowStateObjectImpl {
    async fn get_state(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<DurableWorkflowState, TerminalError> {
        let state: Option<DurableWorkflowState> = ctx.get(STATE_KEY).await.map_err(|e| {
            TerminalError::new(format!("Failed to read state: {}", e))
        })?;

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

        let mut compensations: Vec<CompensationAction> =
            ctx.get(COMPENSATIONS_KEY).await.map_err(|e| {
                TerminalError::new(format!("Failed to read compensations: {}", e))
            })?.unwrap_or_default();

        compensations.push(action);
        ctx.set(COMPENSATIONS_KEY, compensations);

        Ok(())
    }

    async fn get_compensations(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Vec<CompensationAction>, TerminalError> {
        let compensations: Vec<CompensationAction> =
            ctx.get(COMPENSATIONS_KEY).await.map_err(|e| {
                TerminalError::new(format!("Failed to read compensations: {}", e))
            })?.unwrap_or_default();

        Ok(compensations)
    }
}
