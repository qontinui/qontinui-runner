//! Macro Step Handler
//!
//! Handles execution of saved macros by ID. A macro contains
//! multiple action steps that are executed in sequence.

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for macro execution steps.
pub struct MacroHandler;

#[async_trait]
impl StepHandler for MacroHandler {
    fn step_type(&self) -> &'static str {
        "macro"
    }

    fn display_name(&self) -> &'static str {
        "Macro"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        use crate::macros;

        let macro_id = match &step.macro_id {
            Some(id) => id,
            None => return StepHandlerResult::failure("No macro ID specified for macro step"),
        };

        // Get the macro
        let macro_item = match macros::get_macro(macro_id) {
            Some(m) => m,
            None => return StepHandlerResult::failure(format!("Macro not found: {}", macro_id)),
        };

        info!("Executing macro: {} ({})", macro_item.name, macro_id);

        // Increment run count
        if let Err(e) = macros::increment_run_count(macro_id) {
            warn!(
                "Failed to increment run count for macro {}: {}",
                macro_id, e
            );
        }

        let mut all_success = true;
        let mut errors: Vec<String> = Vec::new();

        // Execute each step
        for (idx, macro_step) in macro_item.steps.iter().enumerate() {
            let step_monitor = macro_step.monitor_index.or(step.monitor_index);

            let result = match macro_step.action_type.as_str() {
                "click" | "double_click" | "right_click" => {
                    if let Some(ref image_ids) = macro_step.target_image_ids {
                        if let Some(first_image_id) = image_ids.first() {
                            context
                                .action_service
                                .execute_action(
                                    &macro_step.action_type,
                                    first_image_id,
                                    None,
                                    step_monitor,
                                )
                                .await
                                .map(|r| r.success)
                                .map_err(|e| format!("{:?}", e))
                        } else {
                            Err("No target image specified".to_string())
                        }
                    } else {
                        Err("No target image IDs specified".to_string())
                    }
                }
                "type" => {
                    if let Some(ref text) = macro_step.text_input {
                        let config = json!({"text": text});
                        context
                            .action_service
                            .execute_action("TYPE", "", Some(&config), step_monitor)
                            .await
                            .map(|r| r.success)
                            .map_err(|e| format!("{:?}", e))
                    } else {
                        Err("No text specified for type action".to_string())
                    }
                }
                "hotkey" => {
                    if let Some(ref hotkey) = macro_step.hotkey {
                        let config = json!({"hotkey": hotkey});
                        context
                            .action_service
                            .execute_action("HOTKEY", "", Some(&config), step_monitor)
                            .await
                            .map(|r| r.success)
                            .map_err(|e| format!("{:?}", e))
                    } else {
                        Err("No hotkey specified".to_string())
                    }
                }
                "go_to_state" => {
                    if let Some(ref state_ids) = macro_step.target_state_ids {
                        if let Some(first_state_id) = state_ids.first() {
                            let timeout = macro_step.timeout_seconds;
                            context
                                .action_service
                                .go_to_state(first_state_id, None, step_monitor, timeout)
                                .await
                                .map(|r| r.success)
                                .map_err(|e| format!("{:?}", e))
                        } else {
                            Err("No target state specified".to_string())
                        }
                    } else {
                        Err("No target state IDs specified".to_string())
                    }
                }
                _ => Err(format!("Unknown action type: {}", macro_step.action_type)),
            };

            match result {
                Ok(success) => {
                    if !success {
                        all_success = false;
                        errors.push(format!("Step {} '{}' failed", idx + 1, macro_step.name));
                    }
                }
                Err(e) => {
                    all_success = false;
                    errors.push(format!("Step {} '{}': {}", idx + 1, macro_step.name, e));
                }
            }

            // Apply pause_after_ms if specified
            if let Some(pause_ms) = macro_step.pause_after_ms {
                tokio::time::sleep(tokio::time::Duration::from_millis(pause_ms as u64)).await;
            }
        }

        if all_success {
            StepHandlerResult::success()
        } else {
            StepHandlerResult::failure(errors.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_handler_step_type() {
        let handler = MacroHandler;
        assert_eq!(handler.step_type(), "macro");
        assert_eq!(handler.display_name(), "Macro");
    }
}
