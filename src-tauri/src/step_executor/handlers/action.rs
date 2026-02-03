//! Action Step Handler
//!
//! Handles action execution steps that perform GUI actions on target images.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for action execution steps.
pub struct ActionHandler;

#[async_trait]
impl StepHandler for ActionHandler {
    fn step_type(&self) -> &'static str {
        "action"
    }

    fn display_name(&self) -> &'static str {
        "Action"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let action_type = match &step.action_type {
            Some(t) => t,
            None => return StepHandlerResult::failure("No action type specified"),
        };

        let image_id = match &step.target_image_id {
            Some(id) => id,
            None => return StepHandlerResult::failure("No target image ID specified"),
        };

        match context
            .action_service
            .execute_action(action_type, image_id, None, step.monitor_index)
            .await
        {
            Ok(result) => {
                if result.success {
                    StepHandlerResult::success()
                } else {
                    StepHandlerResult::failure(
                        result
                            .message
                            .unwrap_or_else(|| "Action failed".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Action error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_handler_step_type() {
        let handler = ActionHandler;
        assert_eq!(handler.step_type(), "action");
        assert_eq!(handler.display_name(), "Action");
    }
}
