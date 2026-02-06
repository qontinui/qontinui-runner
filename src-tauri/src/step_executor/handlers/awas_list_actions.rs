//! AWAS List Actions Step Handler
//!
//! Handles awas_list_actions steps that list available AWAS actions.

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use super::awas_common::AwasStepExecutor;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for awas_list_actions steps.
pub struct AwasListActionsHandler;

#[async_trait]
impl StepHandler for AwasListActionsHandler {
    fn step_type(&self) -> &'static str {
        "awas_list_actions"
    }

    fn display_name(&self) -> &'static str {
        "AWAS List Actions"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("AWAS List Actions");
        let timeout = step.timeout_seconds.unwrap_or(60);

        info!("AWAS List Actions");

        let executor = AwasStepExecutor::new(
            context.app_state.clone(),
            context.task_run_id.as_deref(),
            "awas_list_actions",
            step_name,
        );

        let (success, error, data) = executor
            .execute(
                "awas_list_actions",
                None, // No params for list actions
                None,
                None,
                None,
                timeout,
            )
            .await;

        if success {
            let action_count = data
                .as_ref()
                .and_then(|d| d.get("actions"))
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);

            info!("AWAS List Actions: found {} actions", action_count);

            StepHandlerResult::success_with_data(json!({
                "action_count": action_count,
                "data": data,
            }))
        } else {
            StepHandlerResult::failure_with_data(
                error.unwrap_or_else(|| "AWAS list actions failed".to_string()),
                json!({
                    "action_count": 0,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_list_actions_handler_step_type() {
        let handler = AwasListActionsHandler;
        assert_eq!(handler.step_type(), "awas_list_actions");
        assert_eq!(handler.display_name(), "AWAS List Actions");
    }
}
