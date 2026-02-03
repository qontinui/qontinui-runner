//! MCP Call Step Handler
//!
//! Handles mcp_call steps that invoke MCP (Model Context Protocol) server tools.

use async_trait::async_trait;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::mcp_client::CreateMcpCallInput;
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for MCP call steps.
pub struct McpCallHandler;

#[async_trait]
impl StepHandler for McpCallHandler {
    fn step_type(&self) -> &'static str {
        "mcp_call"
    }

    fn display_name(&self) -> &'static str {
        "MCP Call"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Validate required fields
        let server_id = match &step.mcp_server_id {
            Some(id) => id,
            None => {
                return StepHandlerResult::failure("Server ID required for MCP call");
            }
        };

        let tool_name = match &step.mcp_tool_name {
            Some(name) => name,
            None => {
                return StepHandlerResult::failure("Tool name required for MCP call");
            }
        };

        let arguments = step.mcp_arguments.clone().unwrap_or(serde_json::json!({}));
        let fail_on_error = step.mcp_fail_on_error.unwrap_or(true);
        let step_name = step.name.as_deref();

        info!("MCP Call: {}.{}", server_id, tool_name);

        let start_time = std::time::Instant::now();

        // Get the MCP client manager from app state
        let mcp_manager = context.app_state.mcp_client_manager.lock().await;

        // Execute the MCP tool call
        let result = mcp_manager
            .call_tool(server_id, tool_name, arguments.clone())
            .await;
        let duration_ms = start_time.elapsed().as_millis() as i64;

        match result {
            Ok(call_result) => {
                // Save result to database
                Self::save_mcp_call_result(
                    context,
                    server_id,
                    tool_name,
                    &arguments,
                    call_result.content.as_ref(),
                    &call_result.response_type,
                    call_result.success,
                    call_result.error.as_deref(),
                    duration_ms,
                    step_name,
                );

                if call_result.success {
                    info!(
                        "MCP Call '{}.{}' succeeded in {}ms",
                        server_id, tool_name, duration_ms
                    );
                    // Return the content as output data
                    match call_result.content {
                        Some(content) => StepHandlerResult::success_with_data(serde_json::json!({
                            "content": content,
                            "duration_ms": duration_ms,
                        })),
                        None => StepHandlerResult::success(),
                    }
                } else {
                    let error = call_result
                        .error
                        .unwrap_or_else(|| "MCP call failed".to_string());
                    if fail_on_error {
                        StepHandlerResult::failure(error)
                    } else {
                        // Return success but include the error message in data
                        info!(
                            "MCP Call '{}.{}' failed but fail_on_error=false, continuing",
                            server_id, tool_name
                        );
                        StepHandlerResult::success_with_data(serde_json::json!({
                            "ignored_error": error,
                        }))
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("MCP call error: {}", e);

                // Save error result to database
                Self::save_mcp_call_result(
                    context,
                    server_id,
                    tool_name,
                    &arguments,
                    None,
                    "error",
                    false,
                    Some(&error_msg),
                    duration_ms,
                    step_name,
                );

                if fail_on_error {
                    StepHandlerResult::failure(error_msg)
                } else {
                    info!(
                        "MCP Call '{}.{}' errored but fail_on_error=false, continuing",
                        server_id, tool_name
                    );
                    StepHandlerResult::success_with_data(serde_json::json!({
                        "ignored_error": error_msg,
                    }))
                }
            }
        }
    }
}

impl McpCallHandler {
    /// Save an MCP call result to the database (if task_run_id is set)
    fn save_mcp_call_result(
        context: &HandlerContext,
        server_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        response: Option<&serde_json::Value>,
        response_type: &str,
        success: bool,
        error: Option<&str>,
        duration_ms: i64,
        step_name: Option<&str>,
    ) {
        // Only save if we have a task_run_id
        let Some(ref task_run_id) = context.task_run_id else {
            return;
        };

        let input = CreateMcpCallInput {
            task_run_id: task_run_id.clone(),
            step_id: uuid::Uuid::new_v4().to_string(),
            step_name: step_name.map(|s| s.to_string()),
            server_id: server_id.to_string(),
            server_name: None,
            tool_name: tool_name.to_string(),
            arguments: Some(serde_json::to_string(arguments).unwrap_or_default()),
            resolved_arguments: None,
            response: response.map(|r| serde_json::to_string(r).unwrap_or_default()),
            response_type: response_type.to_string(),
            duration_ms,
            extractions: None,
            assertions: None,
            success,
            error_message: error.map(|e| e.to_string()),
        };

        match context
            .app_state
            .checkpoint_db
            .create_task_run_mcp_call(&input)
        {
            Ok(id) => {
                info!(
                    "Saved MCP call result to database: {} (tool: {})",
                    id, tool_name
                );
            }
            Err(e) => {
                warn!("Failed to save MCP call result to database: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_call_handler_step_type() {
        let handler = McpCallHandler;
        assert_eq!(handler.step_type(), "mcp_call");
        assert_eq!(handler.display_name(), "MCP Call");
    }
}
