//! Workflow Step Handler
//!
//! Handles workflow execution steps that run named workflows from the config.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for workflow execution steps.
pub struct WorkflowHandler;

#[async_trait]
impl StepHandler for WorkflowHandler {
    fn step_type(&self) -> &'static str {
        "workflow"
    }

    fn display_name(&self) -> &'static str {
        "Workflow"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let workflow_name = match &step.name {
            Some(name) => name,
            None => return StepHandlerResult::failure("No workflow name specified"),
        };

        // Timeouts are disabled by default - only apply if explicitly specified
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
                            .unwrap_or_else(|| "Workflow failed".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Workflow error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_handler_step_type() {
        let handler = WorkflowHandler;
        assert_eq!(handler.step_type(), "workflow");
        assert_eq!(handler.display_name(), "Workflow");
    }
}
