//! AWAS Execute Step Handler
//!
//! Handles awas_execute steps that execute AWAS actions on target applications.

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use super::awas_common::AwasStepExecutor;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for awas_execute steps.
pub struct AwasExecuteHandler;

#[async_trait]
impl StepHandler for AwasExecuteHandler {
    fn step_type(&self) -> &'static str {
        "awas_execute"
    }

    fn display_name(&self) -> &'static str {
        "AWAS Execute"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("AWAS Execute");
        let timeout = step.timeout_seconds.unwrap_or(60) as u64;

        // Get URL from step config
        let url = match &step.awas_url {
            Some(u) => u,
            None => {
                return StepHandlerResult::failure("No URL specified for awas_execute step");
            }
        };

        // Get action ID
        let action_id = match &step.awas_action_id {
            Some(id) => id,
            None => {
                return StepHandlerResult::failure("No action_id specified for awas_execute step");
            }
        };

        info!("AWAS Execute: {} on {}", action_id, url);

        let command_params = json!({
            "url": url,
            "action_id": action_id,
            "params": step.awas_params.clone(),
        });

        let executor = AwasStepExecutor::new(
            context.app_state.clone(),
            context.task_run_id.as_deref(),
            "awas_execute",
            step_name,
        );

        let (success, error, data) = executor
            .execute(
                "awas_execute",
                Some(command_params.clone()),
                Some(command_params),
                Some(url),
                Some(action_id),
                timeout,
            )
            .await;

        if success {
            StepHandlerResult::success_with_data(json!({
                "url": url,
                "action_id": action_id,
                "executed": true,
                "data": data,
            }))
        } else {
            StepHandlerResult::failure_with_data(
                error.unwrap_or_else(|| "AWAS execute failed".to_string()),
                json!({
                    "url": url,
                    "action_id": action_id,
                    "executed": false,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_execute_handler_step_type() {
        let handler = AwasExecuteHandler;
        assert_eq!(handler.step_type(), "awas_execute");
        assert_eq!(handler.display_name(), "AWAS Execute");
    }
}
