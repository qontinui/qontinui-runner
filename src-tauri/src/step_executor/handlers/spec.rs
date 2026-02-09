//! Spec Step Handler
//!
//! Handles UI Bridge spec verification steps. Sends spec groups to the React
//! frontend for execution against live UI Bridge elements, then collects results.
//!
//! Architecture:
//! ```text
//! Rust (SpecHandler)
//!     | emit("spec-execute-request", { requestId, group, element_source })
//!     v
//! React (useSpecExecutionHandler)
//!     | SpecExecutor.executeGroup(group, elements)
//!     v
//! React emits("spec-execute-response", { requestId, success, result })
//!     | Rust listen("spec-execute-response")
//!     v
//! Oneshot channel delivers result back to handler
//! ```

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::events::TreeEventEmitter;
use crate::step_executor::executor::ExecutionStepConfig;

use once_cell::sync::Lazy;

/// Global pending map for spec execution requests.
///
/// Maps request IDs to oneshot senders. When the React frontend responds
/// via "spec-execute-response", the listener looks up the sender and delivers
/// the response.
pub static SPEC_EXECUTE_PENDING: Lazy<
    Arc<Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
> = Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Default timeout for spec execution (30 seconds)
const SPEC_EXECUTE_TIMEOUT_MS: u64 = 30_000;

/// Handler for UI Bridge spec verification steps.
pub struct SpecHandler;

#[async_trait]
impl StepHandler for SpecHandler {
    fn step_type(&self) -> &'static str {
        "spec"
    }

    fn display_name(&self) -> &'static str {
        "UI Bridge Spec"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Spec Verification");

        // Get spec group JSON from the step config
        let spec_group = match &step.spec_group_json {
            Some(group) => group.clone(),
            None => {
                return StepHandlerResult::failure("No spec_group specified in step config");
            }
        };

        // Get element source (default: "control")
        let element_source = step
            .spec_element_source
            .as_deref()
            .unwrap_or("control")
            .to_string();

        // Get app handle for IPC
        let app_handle = match context.event_emitter.app_handle() {
            Some(handle) => handle.clone(),
            None => {
                return StepHandlerResult::failure(
                    "No app handle available - cannot communicate with React frontend",
                );
            }
        };

        // Generate action ID and emit start event
        let sequence = TreeEventEmitter::next_sequence();
        let action_id = TreeEventEmitter::generate_action_id("spec", sequence);

        let group_name = spec_group
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(step_name);
        let group_id = spec_group
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let metadata = json!({
            "group_name": group_name,
            "group_id": group_id,
            "element_source": element_source,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(
                &action_id,
                &format!("SPEC: {}", group_name),
                metadata.clone(),
            )
            .await;

        // Create request ID and oneshot channel
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<serde_json::Value>();

        // Store sender in pending map
        {
            let mut pending = SPEC_EXECUTE_PENDING.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        // Build the IPC payload
        let mut event_payload = json!({
            "requestId": request_id,
            "group": spec_group,
            "element_source": element_source,
        });

        // Add options if stop_on_failure is set
        if let Some(stop_on_failure) = step.spec_stop_on_failure {
            if let Some(obj) = event_payload.as_object_mut() {
                obj.insert(
                    "options".to_string(),
                    json!({ "stopOnFailure": stop_on_failure }),
                );
            }
        }

        // For external element source, use prefetched elements or fetch from Chrome extension.
        // If fetching fails, fall back to "control" mode (runner's own UI elements).
        if element_source == "external" {
            // Check if elements were pre-fetched by the workflow executor
            let external_elements = if let Some(prefetched) = &step.spec_prefetched_elements {
                info!("Using pre-fetched external elements for spec step");
                Some(prefetched.clone())
            } else {
                // Fall back to fetching via HTTP (may fail if extension not connected)
                match fetch_external_elements().await {
                    Ok(elements) => Some(elements),
                    Err(e) => {
                        warn!(
                            "Failed to fetch external elements for spec '{}': {}. \
                             Falling back to control (runner UI) element source.",
                            group_name, e
                        );
                        None
                    }
                }
            };

            if let Some(elements) = external_elements {
                if let Some(obj) = event_payload.as_object_mut() {
                    obj.insert("elements".to_string(), elements);
                }
            }
            // If external_elements is None, we proceed without injecting elements.
            // The React frontend will use its own control elements (runner UI).
        }

        // Emit request to React frontend
        use tauri::Emitter;
        if let Err(e) = app_handle.emit("spec-execute-request", &event_payload) {
            // Clean up pending entry
            let mut pending = SPEC_EXECUTE_PENDING.lock().await;
            pending.remove(&request_id);

            let error_msg = format!("Failed to emit spec-execute-request: {}", e);
            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("SPEC: {}", group_name),
                    start_timestamp,
                    sequence,
                    &error_msg,
                    Some(metadata),
                )
                .await;

            return StepHandlerResult::failure(error_msg);
        }

        info!(
            "Spec handler: sent request {} for group '{}' (source: {})",
            request_id, group_name, element_source
        );

        // Wait for response with timeout
        let timeout_duration = std::time::Duration::from_millis(SPEC_EXECUTE_TIMEOUT_MS);
        let response = match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                let error_msg = "Spec execution channel closed unexpectedly";
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &format!("SPEC: {}", group_name),
                        start_timestamp,
                        sequence,
                        error_msg,
                        Some(metadata),
                    )
                    .await;
                return StepHandlerResult::failure(error_msg);
            }
            Err(_) => {
                // Timeout - clean up pending entry
                let mut pending = SPEC_EXECUTE_PENDING.lock().await;
                pending.remove(&request_id);

                let error_msg = format!(
                    "Spec execution timed out after {}ms. Is the frontend loaded?",
                    SPEC_EXECUTE_TIMEOUT_MS
                );
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &format!("SPEC: {}", group_name),
                        start_timestamp,
                        sequence,
                        &error_msg,
                        Some(metadata),
                    )
                    .await;
                return StepHandlerResult::failure(error_msg);
            }
        };

        // Parse the response
        let success = response
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !success {
            let error_msg = response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Spec execution failed")
                .to_string();

            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("SPEC: {}", group_name),
                    start_timestamp,
                    sequence,
                    &error_msg,
                    Some(metadata),
                )
                .await;

            return StepHandlerResult::failure(error_msg);
        }

        // Extract the SpecGroupResult
        let result = response.get("result").cloned().unwrap_or(json!(null));

        let passed = result
            .get("passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let passed_count = result
            .get("passedCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let failed_count = result
            .get("failedCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let skipped_count = result
            .get("skippedCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let duration_ms = result
            .get("durationMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let total = passed_count + failed_count + skipped_count;

        // Build output data with full results
        let output_data = json!({
            "spec_result": result,
            "passed": passed,
            "summary": format!("{}/{} assertions passed, {} failed, {} skipped ({}ms)",
                passed_count, total, failed_count, skipped_count, duration_ms),
        });

        let mut result_metadata = metadata.clone();
        if let Some(obj) = result_metadata.as_object_mut() {
            obj.insert("passed".to_string(), json!(passed));
            obj.insert("passed_count".to_string(), json!(passed_count));
            obj.insert("failed_count".to_string(), json!(failed_count));
            obj.insert("duration_ms".to_string(), json!(duration_ms));

            // Include assertion-level results for richer error context.
            // This helps the error monitor provide actionable details about
            // which specific assertions failed and what was expected.
            if let Some(assertions) = result.get("assertions") {
                obj.insert("assertions_detail".to_string(), assertions.clone());
            }

            // Include the spec group definition so error consumers know
            // which spec file and assertions are involved.
            if let Some(spec_name) = spec_group.get("name") {
                obj.insert("spec_group_name".to_string(), spec_name.clone());
            }
            if let Some(spec_id) = spec_group.get("id") {
                obj.insert("spec_group_id".to_string(), spec_id.clone());
            }
        }

        if passed {
            info!(
                "Spec handler: group '{}' PASSED ({}/{} assertions, {}ms)",
                group_name, passed_count, total, duration_ms
            );

            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &format!("SPEC: {}", group_name),
                    start_timestamp,
                    sequence,
                    result_metadata,
                )
                .await;

            StepHandlerResult::success_with_data(output_data)
        } else {
            let summary = format!(
                "{}/{} assertions passed, {} failed ({}ms)",
                passed_count, total, failed_count, duration_ms
            );
            info!("Spec handler: group '{}' FAILED - {}", group_name, summary);

            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("SPEC: {}", group_name),
                    start_timestamp,
                    sequence,
                    &summary,
                    Some(result_metadata),
                )
                .await;

            StepHandlerResult::failure_with_data(summary, output_data)
        }
    }
}

/// Fetch elements from an external browser tab via the Chrome extension HTTP API.
///
/// Sends a "getElements" command to the extension and returns the elements array.
pub async fn fetch_external_elements() -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let payload = json!({
        "action": "getElements",
        "params": {
            "includeNonInteractive": true
        }
    });

    let response = client
        .post("http://127.0.0.1:9876/extension/command")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            format!(
                "UI Bridge spec test failed: Chrome extension not connected. This is NOT a Playwright test - it's a UI Bridge spec verification that requires the Qontinui browser extension to be installed and connected to an active browser tab. ({})",
                e
            )
        })?;

    if !response.status().is_success() {
        return Err(format!(
            "HTTP {}: extension command failed",
            response.status()
        ));
    }

    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let success = data
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !success {
        let error = data
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return Err(format!("Extension command failed: {}", error));
    }

    // Extract elements from response (data.data.elements)
    let elements = data
        .get("data")
        .and_then(|d| d.get("elements"))
        .cloned()
        .unwrap_or(json!([]));

    info!(
        "Fetched {} external elements from Chrome extension",
        elements.as_array().map(|a| a.len()).unwrap_or(0)
    );

    Ok(elements)
}

/// Handle a spec-execute-response event from the React frontend.
///
/// This is called from the Tauri event listener setup to deliver
/// responses to waiting spec handlers.
pub async fn handle_spec_execute_response(response: serde_json::Value) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(request_id) = request_id {
        let mut pending_map = SPEC_EXECUTE_PENDING.lock().await;
        if let Some(sender) = pending_map.remove(&request_id) {
            if sender.send(response).is_err() {
                warn!(
                    "Spec: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("Spec: Delivered response for request {}", request_id);
            }
        } else {
            warn!("Spec: No pending request found for response {}", request_id);
        }
    } else {
        warn!("Spec: Response missing requestId field");
    }
}
