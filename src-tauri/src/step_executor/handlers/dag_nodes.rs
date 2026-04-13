//! DAG Node Step Handlers
//!
//! These handlers recognize DAG-specific step types (`dag_cancel`, `dag_approval`,
//! `dag_loop`) that are emitted by the DAG parser when converting a YAML DAG
//! definition into `ExecutionStepConfig` steps.
//!
//! The actual complex execution logic (loop iteration, approval UI, cancellation
//! propagation) lives in the DAG runtime driver. These handlers exist so the
//! step executor's registry doesn't fall back to the `UnknownStepHandler` when
//! a DAG node is dispatched directly.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use serde_json::json;
use tauri::Emitter;

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

// ============================================================================
// Global approval registry
// ============================================================================

/// Maps approval_id → oneshot sender for pending approval gates.
/// When the frontend invokes `respond_dag_approval`, the sender is removed
/// and fired, unblocking the waiting `DagApprovalHandler::execute` future.
static PENDING_APPROVALS: Lazy<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn register_pending_approval(id: &str, tx: tokio::sync::oneshot::Sender<bool>) {
    let mut map = PENDING_APPROVALS.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(id.to_string(), tx);
}

/// Remove a pending approval from the registry without resolving it.
/// Called on timeout/drop so we don't leak senders.
fn deregister_pending_approval(id: &str) {
    let mut map = PENDING_APPROVALS.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(id);
}

/// Resolve a pending approval gate from outside (called by the Tauri command).
///
/// Returns `Err` if no approval with the given id is pending.
pub fn resolve_approval(id: &str, approved: bool) -> Result<(), String> {
    let mut map = PENDING_APPROVALS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = map.remove(id) {
        tx.send(approved)
            .map_err(|_| "Approval channel already closed".to_string())
    } else {
        Err(format!("No pending approval with id: {}", id))
    }
}

// ============================================================================
// DagCancelHandler
// ============================================================================

/// Handler for `dag_cancel` nodes.
///
/// When a cancel node is reached in a DAG, it terminates the workflow run with
/// a failure result, propagating the cancellation reason recorded in the DAG
/// definition. The cancel reason is stored in `step.name` by the DAG parser.
pub struct DagCancelHandler;

#[async_trait]
impl StepHandler for DagCancelHandler {
    fn step_type(&self) -> &'static str {
        "dag_cancel"
    }

    fn display_name(&self) -> &'static str {
        "DAG Cancel"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        // The DAG parser stores the cancel reason in `step.name` because
        // `ExecutionStepConfig` does not have a dedicated `cancel_reason` field.
        let reason = step
            .name
            .clone()
            .unwrap_or_else(|| "Workflow cancelled".to_string());

        tracing::info!(reason = %reason, "DAG cancel node reached — terminating workflow");

        StepHandlerResult::failure_with_data(
            format!("Workflow cancelled: {}", reason),
            json!({ "cancelled": true, "reason": reason }),
        )
    }
}

// ============================================================================
// DagApprovalHandler
// ============================================================================

/// Handler for `dag_approval` nodes.
///
/// Emits a `dag:approval-requested` Tauri event to the frontend, then blocks
/// until the user responds via the `respond_dag_approval` Tauri command or the
/// timeout elapses (auto-approve on timeout so long runs are never permanently
/// stuck).
pub struct DagApprovalHandler;

#[async_trait]
impl StepHandler for DagApprovalHandler {
    fn step_type(&self) -> &'static str {
        "dag_approval"
    }

    fn display_name(&self) -> &'static str {
        "DAG Approval Gate"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let message = step
            .name
            .clone()
            .unwrap_or_else(|| "Approval required".to_string());

        let approval_id = uuid::Uuid::new_v4().to_string();
        let timeout_secs = step.timeout_seconds.unwrap_or(3600);

        tracing::info!(
            approval_id = %approval_id,
            message = %message,
            timeout_secs = timeout_secs,
            "DAG approval gate — waiting for user response"
        );

        // Emit request to the frontend so the ApprovalDialog can show up.
        if let Some(ref app_handle) = context.app_handle {
            let payload = json!({
                "approval_id": approval_id,
                "message": message,
                "node_name": step.name,
                "task_run_id": context.task_run_id,
            });
            if let Err(e) = app_handle.emit("dag:approval-requested", &payload) {
                tracing::warn!(error = %e, "Failed to emit dag:approval-requested event");
            }
        } else {
            tracing::warn!(
                approval_id = %approval_id,
                "No app_handle available — approval gate auto-approving immediately"
            );
            return StepHandlerResult::success_with_data(json!({
                "approved": true,
                "message": message,
                "auto_approved": true,
                "note": "No Tauri app_handle — headless execution, auto-approved",
            }));
        }

        // Register a oneshot channel so the Tauri command can unblock us.
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        register_pending_approval(&approval_id, tx);

        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(approved)) => {
                if approved {
                    tracing::info!(approval_id = %approval_id, "DAG approval gate — approved by user");
                    StepHandlerResult::success_with_data(json!({
                        "approved": true,
                        "message": message,
                        "auto_approved": false,
                    }))
                } else {
                    tracing::info!(approval_id = %approval_id, "DAG approval gate — rejected by user");
                    StepHandlerResult::failure_with_data(
                        format!("Approval rejected: {}", message),
                        json!({
                            "approved": false,
                            "rejected": true,
                            "message": message,
                        }),
                    )
                }
            }
            Ok(Err(_)) => {
                // Sender was dropped from the registry (shouldn't happen normally).
                tracing::warn!(approval_id = %approval_id, "Approval oneshot channel dropped — auto-approving");
                StepHandlerResult::success_with_data(json!({
                    "approved": true,
                    "message": message,
                    "auto_approved": true,
                    "note": "Approval channel dropped, auto-approved",
                }))
            }
            Err(_timeout) => {
                // Timed out — clean up the registry entry and auto-approve.
                deregister_pending_approval(&approval_id);
                tracing::warn!(
                    approval_id = %approval_id,
                    timeout_secs = timeout_secs,
                    "DAG approval gate timed out — auto-approving"
                );
                StepHandlerResult::success_with_data(json!({
                    "approved": true,
                    "message": message,
                    "auto_approved": true,
                    "note": format!("Approval timed out after {} seconds, auto-approved", timeout_secs),
                }))
            }
        }
    }
}

// ============================================================================
// DagLoopHandler
// ============================================================================

/// Handler for `dag_loop` nodes.
///
/// Loop iteration is managed by the DAG runtime driver, not the step executor.
/// If this handler is invoked directly (e.g., a loop node dispatched outside
/// the DAG context), it returns a pass-through success so execution continues.
pub struct DagLoopHandler;

#[async_trait]
impl StepHandler for DagLoopHandler {
    fn step_type(&self) -> &'static str {
        "dag_loop"
    }

    fn display_name(&self) -> &'static str {
        "DAG Loop"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        tracing::debug!(
            step_name = ?step.name,
            "DAG loop node — pass-through (loop execution is managed by the DAG runtime driver)"
        );

        StepHandlerResult::success_with_data(json!({
            "loop_placeholder": true,
            "note": "Loop execution is managed by the DAG runtime driver",
        }))
    }
}
