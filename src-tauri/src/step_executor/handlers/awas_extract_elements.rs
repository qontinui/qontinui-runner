//! AWAS Extract Elements Step Handler
//!
//! Handles awas_extract_elements steps that extract AWAS elements from HTML content.

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use super::awas_common::AwasStepExecutor;
use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for awas_extract_elements steps.
pub struct AwasExtractElementsHandler;

#[async_trait]
impl StepHandler for AwasExtractElementsHandler {
    fn step_type(&self) -> &'static str {
        "awas_extract_elements"
    }

    fn display_name(&self) -> &'static str {
        "AWAS Extract Elements"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("AWAS Extract Elements");
        let timeout = step.timeout_seconds.unwrap_or(60);

        // Get HTML content from step config
        let html = match &step.awas_html {
            Some(h) => h,
            None => {
                return StepHandlerResult::failure(
                    "No HTML content specified for awas_extract_elements step",
                );
            }
        };

        let base_url = step.awas_base_url.as_deref();

        info!("AWAS Extract Elements (HTML length: {} bytes)", html.len());

        let params = json!({
            "html": html,
            "base_url": base_url,
        });

        // For the database, we don't want to store the full HTML in parameters
        // (could be very large), so we just store metadata
        let params_for_db = json!({
            "html_length": html.len(),
            "base_url": base_url,
        });

        let executor = AwasStepExecutor::new(
            context.app_state.clone(),
            context.task_run_id.as_deref(),
            "awas_extract_elements",
            step_name,
        );

        let (success, error, data) = executor
            .execute(
                "awas_extract_elements",
                Some(params),
                Some(params_for_db),
                base_url,
                None,
                timeout,
            )
            .await;

        if success {
            let element_count = data
                .as_ref()
                .and_then(|d| d.get("elements"))
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);

            info!(
                "AWAS Extract Elements: extracted {} elements",
                element_count
            );

            StepHandlerResult::success_with_data(json!({
                "html_length": html.len(),
                "base_url": base_url,
                "element_count": element_count,
                "data": data,
            }))
        } else {
            StepHandlerResult::failure_with_data(
                error.unwrap_or_else(|| "AWAS extract elements failed".to_string()),
                json!({
                    "html_length": html.len(),
                    "base_url": base_url,
                    "element_count": 0,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_extract_elements_handler_step_type() {
        let handler = AwasExtractElementsHandler;
        assert_eq!(handler.step_type(), "awas_extract_elements");
        assert_eq!(handler.display_name(), "AWAS Extract Elements");
    }
}
