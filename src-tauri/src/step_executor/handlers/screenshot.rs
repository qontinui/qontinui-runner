//! Screenshot Step Handler
//!
//! Handles screenshot capture steps with tree event emission.

use async_trait::async_trait;
use serde_json::json;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::events::TreeEventEmitter;
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for screenshot capture steps.
pub struct ScreenshotHandler;

#[async_trait]
impl StepHandler for ScreenshotHandler {
    fn step_type(&self) -> &'static str {
        "screenshot"
    }

    fn display_name(&self) -> &'static str {
        "Screenshot"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Parse monitor from step config
        let monitor = match &step.screenshot_monitor {
            Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v as i32),
            Some(serde_json::Value::String(s)) if s == "all" => None,
            _ => step.monitor_index,
        };

        // Parse delay
        let delay = if step.screenshot_delay > 0 {
            Some(step.screenshot_delay as f64)
        } else {
            None
        };

        // Generate action ID and emit start event
        let sequence = TreeEventEmitter::next_sequence();
        let action_id = TreeEventEmitter::generate_action_id("screenshot", sequence);

        let metadata = json!({
            "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
            "delay_seconds": delay.unwrap_or(0.0),
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, "SCREENSHOT", metadata.clone())
            .await;

        // Capture the screenshot
        let result = context
            .action_service
            .capture_screenshot(monitor, delay)
            .await;

        match result {
            Ok(res) => {
                let file_path = res.absolute_path.clone().unwrap_or_default();

                // Build result metadata
                let mut result_metadata = metadata.clone();
                if let Some(obj) = result_metadata.as_object_mut() {
                    obj.insert("filename".to_string(), json!(&file_path));
                }

                if res.success {
                    // Emit success event
                    context
                        .event_emitter
                        .emit_action_completed(
                            &action_id,
                            "SCREENSHOT",
                            start_timestamp,
                            sequence,
                            result_metadata,
                        )
                        .await;
                } else {
                    // Emit failure event for unsuccessful capture
                    context
                        .event_emitter
                        .emit_action_failed(
                            &action_id,
                            "SCREENSHOT",
                            start_timestamp,
                            sequence,
                            res.error.as_deref().unwrap_or("Unknown error"),
                            Some(result_metadata),
                        )
                        .await;
                }

                // Record screenshot event for automation logs
                Self::record_screenshot_event(
                    context,
                    "standalone",
                    &file_path,
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    res.success,
                    None,
                    res.error.clone(),
                )
                .await;

                if res.success {
                    StepHandlerResult::success_with_screenshot(file_path)
                } else {
                    StepHandlerResult::failure(
                        res.error.unwrap_or_else(|| "Screenshot failed".to_string()),
                    )
                    .with_screenshot(file_path)
                }
            }
            Err(e) => {
                let error_msg = format!("Screenshot error: {}", e);

                // Emit failure event
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        "SCREENSHOT",
                        start_timestamp,
                        sequence,
                        &error_msg,
                        Some(metadata),
                    )
                    .await;

                // Record failed screenshot event
                Self::record_screenshot_event(
                    context,
                    "standalone",
                    "",
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    false,
                    None,
                    Some(error_msg.clone()),
                )
                .await;

                StepHandlerResult::failure(error_msg)
            }
        }
    }
}

impl ScreenshotHandler {
    /// Record a screenshot event to the run recording handler.
    async fn record_screenshot_event(
        context: &HandlerContext,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<i32>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        let monitor_str = monitor.map(|m| m.to_string());
        context
            .app_state
            .run_recording_handler
            .on_screenshot_captured(
                screenshot_type,
                file_path,
                monitor_str,
                delay_seconds,
                success,
                associated_action,
                error,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screenshot_handler_step_type() {
        let handler = ScreenshotHandler;
        assert_eq!(handler.step_type(), "screenshot");
        assert_eq!(handler.display_name(), "Screenshot");
    }
}
