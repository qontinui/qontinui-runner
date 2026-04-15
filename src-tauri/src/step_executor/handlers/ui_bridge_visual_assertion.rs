use async_trait::async_trait;
use serde_json::json;
use tracing::{error, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// Handler for UI Bridge visual assertion steps.
///
/// Delegates to the ui-bridge-auto visual module endpoints for text assertions,
/// screenshot comparisons, and element highlighting. Supports three assertion types:
///
/// - `"text"`: Assert text content in an element (DOM or OCR-based)
/// - `"screenshot"`: Assert visual regression against a baseline screenshot
/// - `"highlight"`: Highlight an element for visual debugging
pub struct UiBridgeVisualAssertionHandler;

#[async_trait]
impl StepHandler for UiBridgeVisualAssertionHandler {
    fn step_type(&self) -> &'static str {
        "ui_bridge_visual_assertion"
    }

    fn display_name(&self) -> &'static str {
        "UI Bridge Visual Assertion"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let self_base = crate::mcp::types::get_self_base_url(&context.app_state);
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to create HTTP client: {}", e);
                return StepHandlerResult::failure(format!("Failed to create HTTP client: {}", e));
            }
        };

        // Step 1: Check SDK connectivity
        let status_url = format!("{}/ui-bridge/sdk/status", self_base);
        let sdk_connected = match client.get(&status_url).send().await {
            Ok(resp) => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    body.get("data")
                        .and_then(|d| d.get("connected"))
                        .and_then(|c| c.as_bool())
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            Err(e) => {
                warn!("Failed to check SDK status: {}", e);
                false
            }
        };

        if !sdk_connected {
            return StepHandlerResult::failure(
                "SDK is not connected. Connect the target app first.",
            );
        }

        // Step 2: Determine assertion type
        let assertion_type = step.visual_assertion_type.as_deref().unwrap_or("text");

        info!(
            "Running visual assertion: type={}, target={:?}",
            assertion_type,
            step.visual_assertion_expected
                .as_deref()
                .unwrap_or("(none)")
        );

        // Step 3: Build request and call appropriate endpoint
        let (url, body) = match assertion_type {
            "text" => {
                let url = format!("{}/ui-bridge/sdk/auto/assertText", self_base);
                let mut body = json!({});
                if let Some(query) = &step.visual_assertion_query {
                    body["query"] = query.clone();
                }
                if let Some(expected) = &step.visual_assertion_expected {
                    body["expectedText"] = json!(expected);
                }
                if let Some(options) = &step.visual_assertion_options {
                    body["options"] = options.clone();
                }
                (url, body)
            }
            "screenshot" => {
                let url = format!("{}/ui-bridge/sdk/auto/assertScreenshot", self_base);
                let mut body = json!({});
                if let Some(element_id) = &step.visual_assertion_expected {
                    body["elementId"] = json!(element_id);
                }
                if let Some(options) = &step.visual_assertion_options {
                    body["options"] = options.clone();
                }
                (url, body)
            }
            "highlight" => {
                let url = format!("{}/ui-bridge/sdk/auto/highlightElement", self_base);
                let mut body = json!({});
                if let Some(element_id) = &step.visual_assertion_expected {
                    body["elementId"] = json!(element_id);
                }
                if let Some(options) = &step.visual_assertion_options {
                    body["options"] = options.clone();
                }
                (url, body)
            }
            other => {
                return StepHandlerResult::failure(format!(
                    "Unknown visual assertion type: '{}'. Expected 'text', 'screenshot', or 'highlight'.",
                    other
                ));
            }
        };

        // Step 4: Send request
        let response = match client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(body) => body,
                Err(e) => {
                    error!("Failed to parse visual assertion response: {}", e);
                    return StepHandlerResult::failure(format!(
                        "Failed to parse visual assertion response: {}",
                        e
                    ));
                }
            },
            Err(e) => {
                error!("Visual assertion request failed: {}", e);
                return StepHandlerResult::failure(format!(
                    "Visual assertion request failed: {}",
                    e
                ));
            }
        };

        // Step 5: Check response
        let success = response
            .get("success")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        if !success {
            let error_msg = response
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("Unknown error");
            return StepHandlerResult::failure_with_data(
                format!("Visual assertion failed: {}", error_msg),
                response,
            );
        }

        // For text and screenshot assertions, check the pass field in the data
        let data = response.get("data").cloned().unwrap_or(json!({}));

        if assertion_type == "text" || assertion_type == "screenshot" {
            let passed = data.get("pass").and_then(|p| p.as_bool()).unwrap_or(false);

            if !passed {
                let error_detail = data
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        if assertion_type == "text" {
                            let actual = data
                                .get("actualText")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            let expected = data
                                .get("expectedText")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            format!("Expected '{}', got '{}'", expected, actual)
                        } else {
                            let diff = data
                                .get("diffPercentage")
                                .and_then(|d| d.as_f64())
                                .unwrap_or(0.0);
                            format!("Visual regression: {:.2}% pixels differ", diff)
                        }
                    });

                return StepHandlerResult::failure_with_data(
                    format!("Visual assertion did not pass: {}", error_detail),
                    data,
                );
            }
        }

        info!("Visual assertion passed: type={}", assertion_type);
        StepHandlerResult::success_with_data(data)
    }
}
