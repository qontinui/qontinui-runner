//! Playwright Step Handler
//!
//! Handles playwright steps that run Playwright test scripts.
//! Can run scripts by ID or inline content.

use async_trait::async_trait;
use tracing::info;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for Playwright test steps.
pub struct PlaywrightHandler;

#[async_trait]
impl StepHandler for PlaywrightHandler {
    fn step_type(&self) -> &'static str {
        "playwright"
    }

    fn display_name(&self) -> &'static str {
        "Playwright"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        // If we have inline script content (from combined scripts), run it directly
        if let Some(ref script_content) = step.playwright_script_content {
            let script_name = step.name.as_deref().unwrap_or("combined_script");
            Self::run_playwright_inline(
                script_content,
                step.playwright_target_url.as_deref(),
                script_name,
            )
        } else if let Some(ref script_id) = step.playwright_script_id {
            // Otherwise, run by script ID
            Self::run_playwright_script(script_id).await
        } else {
            StepHandlerResult::failure("No Playwright script ID or content specified")
        }
    }
}

impl PlaywrightHandler {
    /// Run inline Playwright script content.
    fn run_playwright_inline(
        content: &str,
        target_url: Option<&str>,
        script_name: &str,
    ) -> StepHandlerResult {
        info!(
            "Running inline Playwright script: {} ({} chars)",
            script_name,
            content.len()
        );

        match crate::playwright::run_script_inline(content, target_url, script_name) {
            Ok(result) => {
                if result.passed {
                    StepHandlerResult::success()
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "Playwright test failed".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Inline Playwright error: {}", e)),
        }
    }

    /// Run a Playwright script by ID via HTTP API.
    async fn run_playwright_script(script_id: &str) -> StepHandlerResult {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:9876/playwright/scripts/{}/run", script_id);

        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    let success = json
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if success {
                        StepHandlerResult::success()
                    } else {
                        let error = json
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "Playwright test failed".to_string());
                        StepHandlerResult::failure(error)
                    }
                } else {
                    StepHandlerResult::failure("Failed to parse Playwright response")
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Playwright request error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playwright_handler_step_type() {
        let handler = PlaywrightHandler;
        assert_eq!(handler.step_type(), "playwright");
        assert_eq!(handler.display_name(), "Playwright");
    }
}
