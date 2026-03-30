//! Step Output Collection Commands
//!
//! Provides Tauri commands for collecting step outputs from workflow executions.
//! These outputs can be used by the test builder to generate verification tests.
//!
//! The step output types match the TypeScript definitions in `src/types/step-output.ts`.

use crate::commands::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tracing::info;

// ============================================================================
// Step Output Types (matching TypeScript definitions)
// ============================================================================

/// Base fields for all step outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseStepOutput {
    pub id: String,
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub step_name: String,
    pub executed_at: String,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// API request step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRequestOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extractions: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_config: Option<serde_json::Value>,
}

/// GUI action step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiActionOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_location: Option<TargetLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_bounds: Option<BoundingBox>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typed_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_pressed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_delta: Option<TargetLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
}

/// Shell command step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommandOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_vars: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extractions: Option<HashMap<String, serde_json::Value>>,
}

/// MCP call step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub server_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Screenshot step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotated_screenshot_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Dimensions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_elements: Option<Vec<DetectedElement>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis_source: Option<String>,
}

/// Playwright script step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightScriptOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_logs: Option<Vec<ConsoleLog>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_data: Option<HashMap<String, serde_json::Value>>,
    /// Test status: "passed", "failed", "pending"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_status: Option<String>,
    /// Assertions that passed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions_passed: Option<i32>,
    /// Assertions that failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions_failed: Option<i32>,
}

/// Check step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub check_type: String,
    pub issues: Vec<CheckIssue>,
    pub checks_passed: i32,
    pub checks_failed: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<String>,
}

/// Workflow reference step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRefOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub workflow_id: String,
    pub workflow_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_outputs: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_variables: Option<HashMap<String, serde_json::Value>>,
}

/// State navigation step output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateNavigationOutput {
    #[serde(flatten)]
    pub base: BaseStepOutput,
    pub target_state: String,
    pub reached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_taken: Option<Vec<PathStep>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_screenshot: Option<String>,
}

// ============================================================================
// Supporting Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetLocation {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dimensions {
    pub width: i32,
    pub height: i32,
}

/// Type alias for backward compatibility — use `vision::types::NormalizedRect` directly.
pub type NormalizedBoundingBox = crate::vision::types::NormalizedRect;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedElement {
    pub id: String,
    pub label: String,
    pub element_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    pub bounding_box: BoundingBox,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized_bounding_box: Option<NormalizedBoundingBox>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleLog {
    #[serde(rename = "type")]
    pub log_type: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckIssue {
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathStep {
    pub from_state: String,
    pub to_state: String,
    pub transition_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_taken: Option<String>,
}

// ============================================================================
// Query Parameters
// ============================================================================

/// Input for collecting step outputs.
#[derive(Debug, Deserialize)]
pub struct CollectStepOutputsInput {
    /// Task run ID to collect outputs from (defaults to most recent)
    #[serde(default)]
    pub task_run_id: Option<String>,
    /// Filter by step types (if empty, returns all)
    #[serde(default)]
    pub step_types: Option<Vec<String>>,
    /// Maximum number of outputs to return
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Response from collecting step outputs.
#[derive(Debug, Serialize)]
pub struct CollectStepOutputsResponse {
    pub success: bool,
    pub task_run_id: Option<String>,
    pub outputs: Vec<serde_json::Value>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Collect step outputs from a workflow execution.
///
/// This command retrieves step outputs that can be used by the test builder
/// to generate verification tests with auto-populated configurations.
///
/// # Arguments
///
/// * `input` - Collection parameters (task_run_id, step_types filter, limit)
///
/// # Returns
///
/// Step outputs in a format compatible with TypeScript StepOutput types.
#[tauri::command]
pub async fn collect_step_outputs(
    state: State<'_, Arc<AppState>>,
    input: CollectStepOutputsInput,
) -> Result<CollectStepOutputsResponse, String> {
    info!("Collecting step outputs: {:?}", input);

    // Determine task run ID
    let task_run_id = if let Some(id) = input.task_run_id {
        id
    } else {
        // Get the most recent task run
        let running_tasks = state
            .checkpoint_db
            .get_running_task_runs(None)
            .map_err(|e| format!("Failed to get running tasks: {}", e))?;

        if let Some(task) = running_tasks.first() {
            task.id.clone()
        } else {
            // Try to get the most recent completed task
            let recent_tasks = state
                .checkpoint_db
                .get_recent_task_runs(1, None)
                .map_err(|e| format!("Failed to list task runs: {}", e))?;

            if let Some(task) = recent_tasks.first() {
                task.id.clone()
            } else {
                return Ok(CollectStepOutputsResponse {
                    success: true,
                    task_run_id: None,
                    outputs: vec![],
                    count: 0,
                    message: Some("No task runs found".to_string()),
                });
            }
        }
    };

    // Get events for this task run
    let events = state
        .pg_db
        .get_task_run_events(&task_run_id, None, input.limit)
        .await
        .map_err(|e| format!("Failed to get task run events: {}", e))?;

    let mut outputs: Vec<serde_json::Value> = Vec::new();

    for event in events {
        // Parse the event data
        let data: Option<serde_json::Value> = event
            .data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let event_type = event.event_type.as_str();
        let event_subtype = event.event_subtype.as_deref().unwrap_or("");

        // Only process completed step events
        if event_subtype != "complete" && event_subtype != "success" && event_subtype != "error" {
            continue;
        }

        // Determine step type from event
        let step_type = data
            .as_ref()
            .and_then(|d| d.get("step_type"))
            .and_then(|v| v.as_str())
            .unwrap_or(event_type);

        // Filter by step types if specified
        if let Some(ref filter_types) = input.step_types {
            if !filter_types.is_empty() && !filter_types.iter().any(|t| t == step_type) {
                continue;
            }
        }

        // Create step output based on type
        let output = create_step_output(&event, &data, step_type);
        if let Some(out) = output {
            outputs.push(out);
        }
    }

    // Also check for Playwright results
    let playwright_results = state
        .pg_db
        .get_task_run_playwright_results(&task_run_id)
        .await
        .unwrap_or_default();

    for result in playwright_results {
        // Filter by step types if specified
        if let Some(ref filter_types) = input.step_types {
            if !filter_types.is_empty()
                && !filter_types
                    .iter()
                    .any(|t| t == "playwright_script" || t == "playwright")
            {
                continue;
            }
        }

        let output = serde_json::json!({
            "id": result.id,
            "step_type": "playwright_script",
            "step_name": result.test_name,
            "executed_at": result.created_at,
            "duration_ms": result.duration_ms.unwrap_or(0),
            "success": result.status == "passed",
            "error": result.error_message,
            "script_content": null,
            "final_url": null,
            "page_title": null,
            "console_logs": parse_console_output(&result.console_output),
            "final_screenshot": result.failure_screenshot_path,
            "test_status": result.status,
            "assertions_passed": result.assertions_passed,
            "assertions_failed": result.assertions_failed,
        });
        outputs.push(output);
    }

    let count = outputs.len();
    info!("Collected {} step outputs for task {}", count, task_run_id);

    Ok(CollectStepOutputsResponse {
        success: true,
        task_run_id: Some(task_run_id),
        outputs,
        count,
        message: None,
    })
}

/// Get step outputs for use in test auto-population.
///
/// This is a convenience command that returns outputs in a format
/// optimized for the test builder's auto-population feature.
#[tauri::command]
pub async fn get_step_outputs_for_test_builder(
    state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let input = CollectStepOutputsInput {
        task_run_id,
        step_types: None,
        limit: Some(50), // Reasonable limit for test builder
    };

    let result = collect_step_outputs(state, input).await?;

    // Transform outputs for test builder consumption
    let transformed: Vec<serde_json::Value> = result
        .outputs
        .into_iter()
        .map(|output| {
            // Add display metadata for UI
            let step_type = output
                .get("step_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let step_name = output
                .get("step_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unnamed Step");
            let success = output
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut enhanced = output.clone();
            if let serde_json::Value::Object(ref mut map) = enhanced {
                map.insert(
                    "display_name".to_string(),
                    serde_json::Value::String(get_display_name(step_type)),
                );
                map.insert(
                    "display_label".to_string(),
                    serde_json::Value::String(format!(
                        "{}: {}",
                        get_display_name(step_type),
                        step_name
                    )),
                );
                map.insert(
                    "status_icon".to_string(),
                    serde_json::Value::String(if success { "✅" } else { "❌" }.to_string()),
                );
            }
            enhanced
        })
        .collect();

    Ok(serde_json::json!({
        "success": true,
        "task_run_id": result.task_run_id,
        "outputs": transformed,
        "count": result.count
    }))
}

// ============================================================================
// Helper Functions
// ============================================================================

fn create_step_output(
    event: &crate::database::TaskRunEvent,
    data: &Option<serde_json::Value>,
    step_type: &str,
) -> Option<serde_json::Value> {
    let base = serde_json::json!({
        "id": event.id,
        "step_type": step_type,
        "step_name": data.as_ref()
            .and_then(|d| d.get("step_name"))
            .and_then(|v| v.as_str())
            .unwrap_or(&event.message),
        "executed_at": event.timestamp,
        "duration_ms": event.duration_ms.unwrap_or(0),
        "success": event.event_subtype.as_deref() != Some("error")
            && event.event_subtype.as_deref() != Some("failed"),
        "error": data.as_ref().and_then(|d| d.get("error")).cloned(),
    });

    match step_type {
        "api_request" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                if let Some(d) = data {
                    map.insert(
                        "method".to_string(),
                        d.get("method").cloned().unwrap_or(serde_json::json!("GET")),
                    );
                    map.insert(
                        "url".to_string(),
                        d.get("url").cloned().unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "status_code".to_string(),
                        d.get("status_code")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "response".to_string(),
                        d.get("response")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "response_headers".to_string(),
                        d.get("response_headers")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "extractions".to_string(),
                        d.get("extractions")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    // Build source_config from data
                    let source_config = serde_json::json!({
                        "method": d.get("method").cloned().unwrap_or(serde_json::json!("GET")),
                        "url": d.get("url").cloned().unwrap_or(serde_json::json!("")),
                        "headers": d.get("request_headers").cloned(),
                        "body": d.get("request_body").cloned(),
                    });
                    map.insert("source_config".to_string(), source_config);
                }
            }
            Some(output)
        }
        "gui_action" | "click" | "double_click" | "right_click" | "type" | "scroll" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert("step_type".to_string(), serde_json::json!("gui_action"));
                if let Some(d) = data {
                    map.insert(
                        "action_type".to_string(),
                        d.get("action_type")
                            .cloned()
                            .unwrap_or(serde_json::json!(step_type)),
                    );
                    map.insert(
                        "target_location".to_string(),
                        d.get("target_location")
                            .or_else(|| d.get("location"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "target_label".to_string(),
                        d.get("target_label")
                            .or_else(|| d.get("template_name"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "confidence".to_string(),
                        d.get("confidence")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "typed_text".to_string(),
                        d.get("typed_text")
                            .or_else(|| d.get("text"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        "command" | "shell_command" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                if let Some(d) = data {
                    map.insert(
                        "command".to_string(),
                        d.get("command").cloned().unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "working_directory".to_string(),
                        d.get("working_directory")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "exit_code".to_string(),
                        d.get("exit_code").cloned().unwrap_or(serde_json::json!(0)),
                    );
                    map.insert(
                        "stdout".to_string(),
                        d.get("stdout").cloned().unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "stderr".to_string(),
                        d.get("stderr").cloned().unwrap_or(serde_json::json!("")),
                    );
                }
            }
            Some(output)
        }
        "mcp_call" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                if let Some(d) = data {
                    map.insert(
                        "server_id".to_string(),
                        d.get("server_id")
                            .or_else(|| d.get("server"))
                            .cloned()
                            .unwrap_or(serde_json::json!("unknown")),
                    );
                    map.insert(
                        "tool_name".to_string(),
                        d.get("tool_name")
                            .or_else(|| d.get("tool"))
                            .cloned()
                            .unwrap_or(serde_json::json!("unknown")),
                    );
                    map.insert(
                        "arguments".to_string(),
                        d.get("arguments")
                            .or_else(|| d.get("args"))
                            .cloned()
                            .unwrap_or(serde_json::json!({})),
                    );
                    map.insert(
                        "result".to_string(),
                        d.get("result")
                            .or_else(|| d.get("response"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "is_error".to_string(),
                        d.get("is_error")
                            .cloned()
                            .unwrap_or(serde_json::json!(false)),
                    );
                }
            }
            Some(output)
        }
        "screenshot" | "capture" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert("step_type".to_string(), serde_json::json!("screenshot"));
                if let Some(d) = data {
                    map.insert(
                        "screenshot_path".to_string(),
                        d.get("screenshot_path")
                            .or_else(|| d.get("path"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "dimensions".to_string(),
                        d.get("dimensions")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "monitor_index".to_string(),
                        d.get("monitor_index")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        "workflow" | "workflow_ref" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert("step_type".to_string(), serde_json::json!("workflow_ref"));
                if let Some(d) = data {
                    map.insert(
                        "workflow_id".to_string(),
                        d.get("workflow_id")
                            .cloned()
                            .unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "workflow_name".to_string(),
                        d.get("workflow_name")
                            .or_else(|| d.get("name"))
                            .cloned()
                            .unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "final_state".to_string(),
                        d.get("final_state")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        "state" | "state_navigation" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert(
                    "step_type".to_string(),
                    serde_json::json!("state_navigation"),
                );
                if let Some(d) = data {
                    map.insert(
                        "target_state".to_string(),
                        d.get("target_state")
                            .or_else(|| d.get("state"))
                            .cloned()
                            .unwrap_or(serde_json::json!("")),
                    );
                    map.insert(
                        "reached".to_string(),
                        d.get("reached").cloned().unwrap_or(serde_json::json!(true)),
                    );
                    map.insert(
                        "final_state".to_string(),
                        d.get("final_state")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        "check" | "lint" | "format" | "typecheck" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert("step_type".to_string(), serde_json::json!("check"));
                if let Some(d) = data {
                    map.insert(
                        "check_type".to_string(),
                        d.get("check_type")
                            .cloned()
                            .unwrap_or(serde_json::json!(step_type)),
                    );
                    map.insert(
                        "issues".to_string(),
                        d.get("issues").cloned().unwrap_or(serde_json::json!([])),
                    );
                    map.insert(
                        "checks_passed".to_string(),
                        d.get("checks_passed")
                            .cloned()
                            .unwrap_or(serde_json::json!(0)),
                    );
                    map.insert(
                        "checks_failed".to_string(),
                        d.get("checks_failed")
                            .cloned()
                            .unwrap_or(serde_json::json!(0)),
                    );
                    map.insert(
                        "raw_output".to_string(),
                        d.get("raw_output")
                            .or_else(|| d.get("output"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        "playwright" | "playwright_script" | "test" | "verification" => {
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                map.insert(
                    "step_type".to_string(),
                    serde_json::json!("playwright_script"),
                );
                if let Some(d) = data {
                    map.insert(
                        "final_url".to_string(),
                        d.get("final_url")
                            .or_else(|| d.get("url"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "page_title".to_string(),
                        d.get("page_title")
                            .or_else(|| d.get("title"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "test_status".to_string(),
                        d.get("status").cloned().unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "assertions_passed".to_string(),
                        d.get("assertions_passed")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    map.insert(
                        "assertions_failed".to_string(),
                        d.get("assertions_failed")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Some(output)
        }
        _ => {
            // For unknown types, return base output with all data
            let mut output = base.clone();
            if let serde_json::Value::Object(ref mut map) = output {
                if let Some(serde_json::Value::Object(data_map)) = data {
                    // Merge all data fields
                    for (key, value) in data_map {
                        if !map.contains_key(key) {
                            map.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            Some(output)
        }
    }
}

fn get_display_name(step_type: &str) -> String {
    match step_type {
        "command" => "Command".to_string(),
        "api_request" => "API Request".to_string(),
        "gui_action" => "GUI Action".to_string(),
        "shell_command" => "Shell Command".to_string(),
        "mcp_call" => "MCP Tool Call".to_string(),
        "screenshot" => "Screenshot".to_string(),
        "workflow_ref" => "Workflow".to_string(),
        "playwright_script" => "Playwright Script".to_string(),
        "state_navigation" => "State Navigation".to_string(),
        "check" => "Check".to_string(),
        _ => step_type.replace('_', " ").to_string(),
    }
}

fn parse_console_output(console_output: &Option<String>) -> Option<Vec<serde_json::Value>> {
    console_output.as_ref().and_then(|output| {
        // Try to parse as JSON array
        if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(output) {
            return Some(parsed);
        }
        // If not JSON, split by newlines and create simple log entries
        let logs: Vec<serde_json::Value> = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::json!({
                    "type": "log",
                    "message": line,
                    "timestamp": ""
                })
            })
            .collect();
        if logs.is_empty() {
            None
        } else {
            Some(logs)
        }
    })
}
