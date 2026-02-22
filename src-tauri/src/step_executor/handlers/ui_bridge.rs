use async_trait::async_trait;
use tracing::{error, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// Handler for UI Bridge steps.
///
/// Performs UI Bridge SDK operations:
/// - navigate: Navigate to a URL
/// - execute: Execute a natural language instruction
/// - assert: Assert a condition on a UI element
/// - snapshot: Capture a snapshot of the current UI state
/// - compare: Compare current snapshot against a reference
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
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let action = step.ui_bridge_action.as_deref().unwrap_or("snapshot");
        let base_url = step
            .ui_bridge_url
            .as_deref()
            .unwrap_or("http://localhost:9876/ui-bridge");
        let timeout_ms =
            step.ui_bridge_timeout_ms
                .unwrap_or(if action == "compare" { 120000 } else { 30000 });

        info!("UI Bridge step: action={}, url={}", action, base_url);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e));

        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(e);
            }
        };

        // Handle compare action separately — it has multi-step logic
        if action == "compare" {
            return self
                .execute_compare(step, context, &client, base_url, timeout_ms)
                .await;
        }

        let result = match action {
            "navigate" => {
                let url = match &step.ui_bridge_url {
                    Some(u) => u.clone(),
                    None => {
                        return StepHandlerResult::failure(
                            "UI Bridge navigate requires 'url' field",
                        );
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
                return StepHandlerResult::failure(format!("Unknown UI Bridge action: {}", other));
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
                StepHandlerResult::failure(format!("UI Bridge {} request failed: {}", action, e))
            }
        }
    }
}

impl UiBridgeHandler {
    /// Execute a "compare" action:
    /// 1. Take a snapshot of the current app state
    /// 2. Load the reference snapshot (from step config or saved snapshot store)
    /// 3. Call the AI compare endpoint
    /// 4. Return ComparisonResult as step output
    /// 5. Pass/fail based on severity_threshold
    async fn execute_compare(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
        client: &reqwest::Client,
        base_url: &str,
        _timeout_ms: u64,
    ) -> StepHandlerResult {
        let comparison_mode = step
            .ui_bridge_compare_mode
            .as_deref()
            .unwrap_or("structural");
        let severity_threshold = step
            .ui_bridge_severity_threshold
            .as_deref()
            .unwrap_or("major");

        // Step 1: Take a snapshot of the current app state
        info!("Compare: taking target snapshot...");
        let snapshot_endpoint = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
        let target_snapshot = match client.get(&snapshot_endpoint).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                serde_json::from_str::<serde_json::Value>(&body)
                    .unwrap_or(serde_json::json!({ "raw": body }))
            }
            Ok(resp) => {
                let status = resp.status();
                return StepHandlerResult::failure(format!(
                    "Failed to take target snapshot: HTTP {}",
                    status
                ));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to take target snapshot: {}",
                    e
                ));
            }
        };

        // Step 2: Load reference snapshot
        let reference_snapshot = if let Some(ref snap) = step.ui_bridge_reference_snapshot {
            // Inline reference snapshot provided in step config
            snap.clone()
        } else if let Some(ref snap_id) = step.ui_bridge_reference_snapshot_id {
            // Load from saved snapshots via runner API
            info!("Compare: loading saved reference snapshot {}...", snap_id);
            let snap_endpoint = format!("http://localhost:9876/comparison-snapshots/{}", snap_id);
            match client.get(&snap_endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.unwrap_or_default();
                    let saved: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    // The saved snapshot has snapshot_data field
                    saved.get("snapshot_data").cloned().unwrap_or(saved)
                }
                Ok(resp) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to load reference snapshot {}: HTTP {}",
                        snap_id,
                        resp.status()
                    ));
                }
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to load reference snapshot {}: {}",
                        snap_id, e
                    ));
                }
            }
        } else {
            return StepHandlerResult::failure(
                "Compare action requires either reference_snapshot or reference_snapshot_id",
            );
        };

        // Step 3: Call AI comparison endpoint
        info!(
            "Compare: running AI comparison (mode={})...",
            comparison_mode
        );
        let compare_endpoint = "http://localhost:9876/ai/compare-snapshots";
        let compare_body = serde_json::json!({
            "reference_snapshot": reference_snapshot,
            "target_snapshot": target_snapshot,
            "comparison_mode": comparison_mode,
        });

        let comparison_result = match client
            .post(compare_endpoint)
            .json(&compare_body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                serde_json::from_str::<serde_json::Value>(&body)
                    .unwrap_or(serde_json::json!({ "error": "Failed to parse comparison result" }))
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return StepHandlerResult::failure(format!(
                    "AI comparison failed: HTTP {} - {}",
                    status, body
                ));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!("AI comparison request failed: {}", e));
            }
        };

        // Step 4: Evaluate pass/fail based on severity threshold
        let threshold_order = match severity_threshold {
            "critical" => 4,
            "major" => 3,
            "minor" => 2,
            "info" => 1,
            _ => 3, // default to major
        };

        let critical_count = comparison_result
            .get("criticalCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let major_count = comparison_result
            .get("majorCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let minor_count = comparison_result
            .get("minorCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let info_count = comparison_result
            .get("infoCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let failing_count = match threshold_order {
            4 => critical_count,
            3 => critical_count + major_count,
            2 => critical_count + major_count + minor_count,
            1 => critical_count + major_count + minor_count + info_count,
            _ => critical_count + major_count,
        };

        let total_differences = comparison_result
            .get("totalDifferences")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let summary = comparison_result
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Comparison completed")
            .to_string();

        // Build output with comparison_result embedded for frontend rendering
        let output_data = serde_json::json!({
            "comparison_result": comparison_result,
            "severity_threshold": severity_threshold,
            "failing_count": failing_count,
            "total_differences": total_differences,
        });

        if failing_count > 0 {
            warn!(
                "Compare: {} findings at or above '{}' threshold (total: {})",
                failing_count, severity_threshold, total_differences
            );
            StepHandlerResult::failure_with_data(
                format!(
                    "Comparison found {} finding(s) at or above '{}' severity: {}",
                    failing_count, severity_threshold, summary
                ),
                output_data,
            )
        } else {
            info!(
                "Compare: passed (total differences: {}, none at or above '{}')",
                total_differences, severity_threshold
            );
            StepHandlerResult::success_with_data(output_data)
        }
    }
}
