use async_trait::async_trait;
use tracing::{error, info};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// Handler for UI Bridge steps.
///
/// Performs UI Bridge SDK operations:
/// - navigate: Navigate to a URL
/// - execute: Execute a natural language instruction
/// - assert: Assert a condition on a UI element
/// - snapshot: Capture a snapshot of the current UI state
pub struct UiBridgeHandler;

#[async_trait]
impl StepHandler for UiBridgeHandler {
    fn step_type(&self) -> &'static str {
        "ui_bridge"
    }

    fn display_name(&self) -> &'static str {
        "UI Bridge"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        let action = step.ui_bridge_action.as_deref().unwrap_or("snapshot");
        let base_url = step
            .ui_bridge_url
            .as_deref()
            .unwrap_or("http://localhost:9876/ui-bridge");
        let timeout_ms = step.ui_bridge_timeout_ms.unwrap_or(30000);

        info!("UI Bridge step: action={}, url={}", action, base_url);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e));

        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult {
                    success: false,
                    error: Some(e),
                    output_data: None,
                    screenshot_path: None,
                };
            }
        };

        let result = match action {
            "navigate" => {
                let url = match &step.ui_bridge_url {
                    Some(u) => u.clone(),
                    None => {
                        return StepHandlerResult {
                            success: false,
                            error: Some("UI Bridge navigate requires 'url' field".to_string()),
                            output_data: None,
                            screenshot_path: None,
                        };
                    }
                };
                let endpoint = format!("{}/sdk/navigate", base_url.trim_end_matches('/'));
                let body = serde_json::json!({ "url": url });
                client.post(&endpoint).json(&body).send().await
            }
            "execute" => {
                let instruction = step.ui_bridge_instruction.as_deref().unwrap_or("");
                let endpoint = format!("{}/sdk/execute", base_url.trim_end_matches('/'));
                let mut body = serde_json::json!({ "instruction": instruction });
                if let Some(timeout) = step.ui_bridge_timeout_ms {
                    body["timeout"] = serde_json::json!(timeout);
                }
                client.post(&endpoint).json(&body).send().await
            }
            "assert" => {
                let target = step.ui_bridge_target.as_deref().unwrap_or("");
                let assert_type = step.ui_bridge_assert_type.as_deref().unwrap_or("exists");
                let expected = step.ui_bridge_expected.as_deref();
                let endpoint = format!("{}/sdk/assert", base_url.trim_end_matches('/'));
                let mut body = serde_json::json!({
                    "target": target,
                    "type": assert_type,
                });
                if let Some(exp) = expected {
                    body["expected"] = serde_json::json!(exp);
                }
                client.post(&endpoint).json(&body).send().await
            }
            "snapshot" => {
                let endpoint = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
                client.get(&endpoint).send().await
            }
            other => {
                return StepHandlerResult {
                    success: false,
                    error: Some(format!("Unknown UI Bridge action: {}", other)),
                    output_data: None,
                    screenshot_path: None,
                };
            }
        };

        match result {
            Ok(response) => {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();

                let output_data: serde_json::Value = serde_json::from_str(&body_text)
                    .unwrap_or(serde_json::json!({ "raw_response": body_text }));

                let success = status.is_success();
                let error = if !success {
                    Some(format!(
                        "UI Bridge {} returned status {}: {}",
                        action, status, body_text
                    ))
                } else {
                    None
                };

                info!(
                    "UI Bridge {} completed: success={}, status={}",
                    action, success, status
                );

                StepHandlerResult {
                    success,
                    error,
                    output_data: Some(output_data),
                    screenshot_path: None,
                }
            }
            Err(e) => {
                error!("UI Bridge {} request failed: {}", action, e);
                StepHandlerResult {
                    success: false,
                    error: Some(format!("UI Bridge {} request failed: {}", action, e)),
                    output_data: None,
                    screenshot_path: None,
                }
            }
        }
    }
}
