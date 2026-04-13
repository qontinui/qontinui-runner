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

use async_trait::async_trait;
use serde_json::json;

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

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
/// An approval gate pauses DAG execution until a user approves or rejects the
/// checkpoint. The full approval-gate UI is not yet implemented; this handler
/// auto-approves and logs the request so that existing DAG runs are not blocked.
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
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        let message = step
            .name
            .clone()
            .unwrap_or_else(|| "Approval required".to_string());

        tracing::info!(
            message = %message,
            "DAG approval gate — auto-approving (approval gate UI not yet implemented)"
        );

        StepHandlerResult::success_with_data(json!({
            "approved": true,
            "message": message,
            "auto_approved": true,
        }))
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
