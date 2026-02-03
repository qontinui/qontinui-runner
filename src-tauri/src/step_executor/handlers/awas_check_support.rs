//! AWAS Check Support Step Handler
//!
//! Handles awas_check_support steps that check if a URL supports AWAS.

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use super::awas_common::AwasStepExecutor;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for awas_check_support steps.
pub struct AwasCheckSupportHandler;

#[async_trait]
impl StepHandler for AwasCheckSupportHandler {
    fn step_type(&self) -> &'static str {
        "awas_check_support"
    }

    fn display_name(&self) -> &'static str {
        "AWAS Check Support"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("AWAS Check Support");
        let timeout = step.timeout_seconds.unwrap_or(60) as u64;

        // Get URL from step config
        let url = match &step.awas_url {
            Some(u) => u,
            None => {
                return StepHandlerResult::failure("No URL specified for awas_check_support step");
            }
        };

        info!("AWAS Check Support: {}", url);

        let params = json!({
            "url": url,
        });

        let executor = AwasStepExecutor::new(
            context.app_state.clone(),
            context.task_run_id.as_deref(),
            "awas_check_support",
            step_name,
        );

        let (success, error, data) = executor
            .execute(
                "awas_check_support",
                Some(params.clone()),
                Some(params),
                Some(url),
                None,
                timeout,
            )
            .await;

        if success {
            // Extract support status from response data
            let supported = data
                .as_ref()
                .and_then(|d| d.get("supported"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            info!(
                "AWAS is {} at {}",
                if supported { "supported" } else { "not supported" },
                url
            );

            StepHandlerResult::success_with_data(json!({
                "url": url,
                "supported": supported,
                "data": data,
            }))
        } else {
            StepHandlerResult::failure_with_data(
                error.unwrap_or_else(|| "AWAS check support failed".to_string()),
                json!({
                    "url": url,
                    "supported": false,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_check_support_handler_step_type() {
        let handler = AwasCheckSupportHandler;
        assert_eq!(handler.step_type(), "awas_check_support");
        assert_eq!(handler.display_name(), "AWAS Check Support");
    }
}
