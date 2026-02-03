//! State Step Handler
//!
//! Handles state navigation steps that transition to a target state.

use async_trait::async_trait;
use tracing::info;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for state navigation steps.
pub struct StateHandler;

#[async_trait]
impl StepHandler for StateHandler {
    fn step_type(&self) -> &'static str {
        "state"
    }

    fn display_name(&self) -> &'static str {
        "State"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let state_name = match &step.name {
            Some(name) => name,
            None => return StepHandlerResult::failure("No state name specified"),
        };

        // Timeouts are disabled by default - only apply if explicitly specified
        let timeout = step.timeout_seconds;

        match context
            .action_service
            .go_to_state(state_name, None, step.monitor_index, timeout)
            .await
        {
            Ok(result) => {
                if result.success {
                    info!(
                        "GO_TO_STATE '{}': Success. Check Python logs for details \
                        (transition may have been skipped if state was already active)",
                        state_name
                    );
                    StepHandlerResult::success()
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "State navigation failed".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("State navigation error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_handler_step_type() {
        let handler = StateHandler;
        assert_eq!(handler.step_type(), "state");
        assert_eq!(handler.display_name(), "State");
    }
}
