//! GUI Action Step Handler
//!
//! Handles gui_action steps that perform vision-based GUI automation.
//! This is similar to the action step type but specifically for GUI interactions.

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for GUI action steps.
pub struct GuiActionHandler;

#[async_trait]
impl StepHandler for GuiActionHandler {
    fn step_type(&self) -> &'static str {
        "gui_action"
    }

    fn display_name(&self) -> &'static str {
        "GUI Action"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let action_type = match &step.action_type {
            Some(t) => t,
            None => return StepHandlerResult::failure("No action type specified for GUI action"),
        };

        // Check if we have a target image ID
        if let Some(ref image_id) = step.target_image_id {
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
                                .unwrap_or_else(|| "GUI action failed".to_string()),
                        )
                    }
                }
                Err(e) => StepHandlerResult::failure(format!("GUI action error: {}", e)),
            }
        } else {
            // For type/hotkey actions that don't need a target image
            match action_type.as_str() {
                "type" | "hotkey" | "scroll" => StepHandlerResult::failure(format!(
                    "GUI action '{}' not yet implemented in step executor",
                    action_type
                )),
                _ => StepHandlerResult::failure("No target image ID specified for GUI action"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gui_action_handler_step_type() {
        let handler = GuiActionHandler;
        assert_eq!(handler.step_type(), "gui_action");
        assert_eq!(handler.display_name(), "GUI Action");
    }
}
