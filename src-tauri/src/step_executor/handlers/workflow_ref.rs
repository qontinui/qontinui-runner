//! Workflow Reference Step Handler
//!
//! Handles workflow_ref steps that run another workflow by reference/ID.
//! This is essentially an alias for the workflow step type.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for workflow reference steps.
pub struct WorkflowRefHandler;

#[async_trait]
impl StepHandler for WorkflowRefHandler {
    fn step_type(&self) -> &'static str {
        "workflow_ref"
    }

    fn display_name(&self) -> &'static str {
        "Workflow Reference"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let workflow_name = match &step.name {
            Some(name) => name,
            None => {
                return StepHandlerResult::failure("No workflow name specified for workflow_ref")
            }
        };

        let timeout = step.timeout_seconds;

        match context
            .action_service
            .run_workflow(
                workflow_name,
                None,
                step.monitor_index,
                timeout,
                step.initial_state_ids.as_deref(),
            )
            .await
        {
            Ok(result) => {
                if result.success {
                    StepHandlerResult::success()
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "Workflow ref failed".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Workflow ref error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_ref_handler_step_type() {
        let handler = WorkflowRefHandler;
        assert_eq!(handler.step_type(), "workflow_ref");
        assert_eq!(handler.display_name(), "Workflow Reference");
    }
}
