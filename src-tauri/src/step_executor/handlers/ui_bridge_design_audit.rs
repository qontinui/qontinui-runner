use async_trait::async_trait;
use serde_json::json;
use tracing::{error, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// Handler for UI Bridge design audit verification steps.
///
/// Checks SDK connectivity, then runs the design audit endpoint to detect
/// contrast issues, select option visibility problems, and accessibility violations.
/// Fails the step if any error-severity issues are found.
pub struct UiBridgeDesignAuditHandler;

#[async_trait]
impl StepHandler for UiBridgeDesignAuditHandler {
    fn step_type(&self) -> &'static str {
        "ui_bridge_design_audit"
    }

    fn display_name(&self) -> &'static str {
        "UI Bridge Design Audit"
    }

    async fn execute(
        &self,
        _step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let self_base = crate::mcp::types::get_self_base_url(&context.app_state);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

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

        info!("SDK connected, running design audit");

        // Step 2: Call the design audit endpoint
        let audit_url = format!("{}/ui-bridge/sdk/ai/design-audit", self_base);
        let audit_result = client
            .post(&audit_url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await;

        let response_body = match audit_result {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(body) => body,
                Err(e) => {
                    error!("Failed to parse design audit response: {}", e);
                    return StepHandlerResult::failure(format!(
                        "Failed to parse design audit response: {}",
                        e
                    ));
                }
            },
            Err(e) => {
                error!("Design audit request failed: {}", e);
                return StepHandlerResult::failure(format!(
                    "Design audit request failed: {}",
                    e
                ));
            }
        };

        // Step 3: Parse issues from the response
        // Response may be wrapped in { data: { issues: [...] } } or { issues: [...] }
        let issues = response_body
            .get("data")
            .and_then(|d| d.get("issues"))
            .or_else(|| response_body.get("issues"))
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();

        let error_issues: Vec<&serde_json::Value> = issues
            .iter()
            .filter(|issue| {
                issue
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .map(|s| s == "error")
                    .unwrap_or(false)
            })
            .collect();

        let warning_count = issues
            .iter()
            .filter(|issue| {
                issue
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .map(|s| s == "warning")
                    .unwrap_or(false)
            })
            .count();

        let info_count = issues.len() - error_issues.len() - warning_count;

        let summary = json!({
            "total_issues": issues.len(),
            "error_count": error_issues.len(),
            "warning_count": warning_count,
            "info_count": info_count,
            "issues": issues,
        });

        if !error_issues.is_empty() {
            let error_descriptions: Vec<String> = error_issues
                .iter()
                .map(|issue| {
                    let element = issue
                        .get("element")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown");
                    let desc = issue
                        .get("issue")
                        .and_then(|i| i.as_str())
                        .unwrap_or("unknown issue");
                    format!("  - {}: {}", element, desc)
                })
                .collect();

            StepHandlerResult::failure_with_data(
                format!(
                    "Design audit found {} error-severity issue(s):\n{}",
                    error_issues.len(),
                    error_descriptions.join("\n")
                ),
                summary,
            )
        } else {
            info!(
                "Design audit passed: {} warnings, {} info",
                warning_count, info_count
            );
            StepHandlerResult::success_with_data(summary)
        }
    }
}
