//! AWAS Discover Step Handler
//!
//! Handles awas_discover steps that discover AWAS manifests from URLs.

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use super::awas_common::AwasStepExecutor;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for awas_discover steps.
pub struct AwasDiscoverHandler;

#[async_trait]
impl StepHandler for AwasDiscoverHandler {
    fn step_type(&self) -> &'static str {
        "awas_discover"
    }

    fn display_name(&self) -> &'static str {
        "AWAS Discover"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("AWAS Discover");
        let timeout = step.timeout_seconds.unwrap_or(60) as u64;

        // Get URL from step config
        let url = match &step.awas_url {
            Some(u) => u,
            None => {
                return StepHandlerResult::failure("No URL specified for awas_discover step");
            }
        };

        info!("AWAS Discover: {}", url);

        let params = json!({
            "url": url,
        });

        let executor = AwasStepExecutor::new(
            context.app_state.clone(),
            context.task_run_id.as_deref(),
            "awas_discover",
            step_name,
        );

        let (success, error, data) = executor
            .execute(
                "awas_discover",
                Some(params.clone()),
                Some(params),
                Some(url),
                None,
                timeout,
            )
            .await;

        if success {
            StepHandlerResult::success_with_data(json!({
                "url": url,
                "discovered": true,
                "data": data,
            }))
        } else {
            StepHandlerResult::failure_with_data(
                error.unwrap_or_else(|| "AWAS discover failed".to_string()),
                json!({
                    "url": url,
                    "discovered": false,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_discover_handler_step_type() {
        let handler = AwasDiscoverHandler;
        assert_eq!(handler.step_type(), "awas_discover");
        assert_eq!(handler.display_name(), "AWAS Discover");
    }
}
