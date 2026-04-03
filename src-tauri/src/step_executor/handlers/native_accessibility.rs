//! Native Accessibility step handler.
//!
//! Drives desktop automation through the Rust `AccessibilityManager` directly,
//! bypassing any Python HAL layer. Supports capturing the accessibility tree,
//! clicking, typing, focusing, querying, and generating AI-friendly context.

use async_trait::async_trait;
use serde_json::json;
use tauri::Manager;
use tracing::{debug, info};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;
use qontinui_runner_lib::accessibility::model::UnifiedRole;
use qontinui_runner_lib::accessibility::traits::ConnectionTarget;
use qontinui_runner_lib::accessibility::AccessibilityManager;

/// Default timeout for connecting to an accessibility source (ms).
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5000;

/// Default max elements for ai_context output.
const DEFAULT_MAX_ELEMENTS: usize = 50;

/// Type alias for the managed accessibility state.
type A11yState = tokio::sync::Mutex<AccessibilityManager>;

/// Step handler for native desktop accessibility operations.
///
/// Reads action-specific parameters from the `ExecutionStepConfig` fields
/// prefixed with `a11y_`. Uses the `AccessibilityManager` stored as Tauri
/// managed state behind a `tokio::sync::Mutex`.
pub struct NativeAccessibilityHandler;

impl NativeAccessibilityHandler {
    /// Parse the `target` field into a `ConnectionTarget`.
    fn parse_target(target: &str) -> ConnectionTarget {
        if target.eq_ignore_ascii_case("desktop") {
            ConnectionTarget::Desktop
        } else if let Some(pid_str) = target.strip_prefix("pid:") {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                ConnectionTarget::ProcessId(pid)
            } else {
                // Fall back to window-title matching if the PID is not numeric.
                ConnectionTarget::WindowTitle(target.to_string())
            }
        } else {
            ConnectionTarget::WindowTitle(target.to_string())
        }
    }
}

/// Resolve the managed `AccessibilityManager` state from the handler context.
///
/// Returns a reference to the `tokio::sync::Mutex<AccessibilityManager>`.
/// Caller must `.lock().await` to obtain the guard.
macro_rules! get_a11y_state {
    ($context:expr) => {{
        let app_handle = match $context.app_handle.as_ref() {
            Some(h) => h,
            None => {
                return StepHandlerResult::failure(
                    "No app handle available — native accessibility requires a Tauri runtime",
                );
            }
        };
        match app_handle.try_state::<A11yState>() {
            Some(s) => s,
            None => {
                return StepHandlerResult::failure(
                    "AccessibilityManager not registered in Tauri managed state",
                );
            }
        }
    }};
}

#[async_trait]
impl StepHandler for NativeAccessibilityHandler {
    fn step_type(&self) -> &'static str {
        "native_accessibility"
    }

    fn display_name(&self) -> &'static str {
        "Native Accessibility"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Check the feature flag — if use_rust_accessibility is disabled, reject the step
        let settings = crate::settings::get_accessibility_settings();
        if !settings.use_rust_accessibility {
            return StepHandlerResult::failure(
                "Native accessibility is disabled. \
                 Enable 'use_rust_accessibility' in Accessibility Settings to use this step type.",
            );
        }

        let action = match step.a11y_action.as_deref() {
            Some(a) => a,
            None => {
                return StepHandlerResult::failure(
                    "Missing 'a11y_action' field. \
                     Valid actions: capture, click, type, focus, query, ai_context",
                )
            }
        };

        info!(
            "Native accessibility action: '{}' (step: {})",
            action,
            step.name.as_deref().unwrap_or("unnamed")
        );

        match action {
            "capture" => self.action_capture(step, context).await,
            "click" => self.action_click(step, context).await,
            "type" => self.action_type(step, context).await,
            "focus" => self.action_focus(step, context).await,
            "query" => self.action_query(step, context).await,
            "ai_context" => self.action_ai_context(step, context).await,
            unknown => StepHandlerResult::failure(format!(
                "Unknown a11y action: '{}'. \
                 Valid actions: capture, click, type, focus, query, ai_context",
                unknown
            )),
        }
    }
}

impl NativeAccessibilityHandler {
    // ------------------------------------------------------------------
    // Action implementations
    // ------------------------------------------------------------------

    async fn action_capture(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let state = get_a11y_state!(context);
        let mut mgr = state.lock().await;

        // Auto-connect if not connected and a target is specified.
        if !mgr.is_connected() {
            let target_str = step.a11y_target.as_deref().unwrap_or("Desktop");
            let target = Self::parse_target(target_str);
            info!("Auto-connecting to accessibility source: {:?}", target);
            if let Err(e) = mgr.connect(target, DEFAULT_CONNECT_TIMEOUT_MS).await {
                return StepHandlerResult::failure(format!(
                    "Failed to connect to accessibility source: {}",
                    e
                ));
            }
        }

        let max_depth = step.a11y_max_depth;
        let include_hidden = step.a11y_include_hidden.unwrap_or(false);

        match mgr.capture(max_depth, include_hidden).await {
            Ok(snapshot) => {
                let summary = json!({
                    "total_nodes": snapshot.total_nodes,
                    "interactive_nodes": snapshot.interactive_nodes,
                    "title": snapshot.title,
                    "url": snapshot.url,
                    "source": format!("{:?}", snapshot.source),
                });
                info!(
                    "Captured accessibility tree: {} nodes ({} interactive)",
                    snapshot.total_nodes, snapshot.interactive_nodes
                );
                StepHandlerResult::success_with_data(summary)
            }
            Err(e) => StepHandlerResult::failure(format!("Capture failed: {}", e)),
        }
    }

    async fn action_click(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let ref_id = match step.a11y_ref_id.as_deref() {
            Some(r) => r,
            None => {
                return StepHandlerResult::failure(
                    "Missing 'a11y_ref_id' — click requires a ref ID (e.g. \"@e3\")",
                )
            }
        };

        let state = get_a11y_state!(context);
        let mgr = state.lock().await;

        if !mgr.is_connected() {
            return StepHandlerResult::failure("Not connected to accessibility source");
        }

        match mgr.click(ref_id).await {
            Ok(result) => {
                debug!("Click result: {:?}", result);
                if result.success {
                    StepHandlerResult::success_with_data(
                        serde_json::to_value(&result).unwrap_or(json!({"success": true})),
                    )
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "Click failed (unknown error)".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Click failed: {}", e)),
        }
    }

    async fn action_type(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let ref_id = match step.a11y_ref_id.as_deref() {
            Some(r) => r,
            None => {
                return StepHandlerResult::failure(
                    "Missing 'a11y_ref_id' — type requires a ref ID (e.g. \"@e5\")",
                )
            }
        };
        let text = match step.a11y_text.as_deref() {
            Some(t) => t,
            None => {
                return StepHandlerResult::failure(
                    "Missing 'a11y_text' — type action requires text to enter",
                )
            }
        };

        let clear_first = step.a11y_clear_first.unwrap_or(false);

        let state = get_a11y_state!(context);
        let mgr = state.lock().await;

        if !mgr.is_connected() {
            return StepHandlerResult::failure("Not connected to accessibility source");
        }

        match mgr.type_text(ref_id, text, clear_first).await {
            Ok(result) => {
                debug!("Type result: {:?}", result);
                if result.success {
                    StepHandlerResult::success_with_data(
                        serde_json::to_value(&result).unwrap_or(json!({"success": true})),
                    )
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "Type failed (unknown error)".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Type failed: {}", e)),
        }
    }

    async fn action_focus(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let ref_id = match step.a11y_ref_id.as_deref() {
            Some(r) => r,
            None => {
                return StepHandlerResult::failure(
                    "Missing 'a11y_ref_id' — focus requires a ref ID (e.g. \"@e2\")",
                )
            }
        };

        let state = get_a11y_state!(context);
        let mgr = state.lock().await;

        if !mgr.is_connected() {
            return StepHandlerResult::failure("Not connected to accessibility source");
        }

        match mgr.focus(ref_id).await {
            Ok(result) => {
                debug!("Focus result: {:?}", result);
                if result.success {
                    StepHandlerResult::success_with_data(
                        serde_json::to_value(&result).unwrap_or(json!({"success": true})),
                    )
                } else {
                    StepHandlerResult::failure(
                        result
                            .error
                            .unwrap_or_else(|| "Focus failed (unknown error)".to_string()),
                    )
                }
            }
            Err(e) => StepHandlerResult::failure(format!("Focus failed: {}", e)),
        }
    }

    async fn action_query(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let state = get_a11y_state!(context);
        let mgr = state.lock().await;

        if !mgr.is_connected() {
            return StepHandlerResult::failure("Not connected to accessibility source");
        }

        let snapshot = match mgr.snapshot().await {
            Some(s) => s,
            None => {
                return StepHandlerResult::failure(
                    "No cached accessibility snapshot — run a 'capture' action first",
                )
            }
        };

        // Build query from step fields.
        let mut builder = mgr.query();

        if let Some(ref role_str) = step.a11y_query_role {
            // Deserialize the role string via serde (uses rename_all = "snake_case").
            match serde_json::from_value::<UnifiedRole>(serde_json::Value::String(role_str.clone()))
            {
                Ok(role) => {
                    builder = builder.by_role(role);
                }
                Err(_) => {
                    return StepHandlerResult::failure(format!(
                        "Unknown accessibility role: '{}'. \
                         Use snake_case role names (e.g. 'button', 'textbox').",
                        role_str
                    ));
                }
            }
        }

        if let Some(ref label) = step.a11y_query_label {
            builder = builder.by_label(label.as_str());
        }

        if step.a11y_interactive_only.unwrap_or(false) {
            builder = builder.interactive();
        }

        let results = builder.find_all(&snapshot.root);

        let nodes_json: Vec<serde_json::Value> = results
            .iter()
            .map(|node| {
                json!({
                    "ref": node.ref_id,
                    "role": node.role.as_str(),
                    "name": node.name,
                    "value": node.value,
                    "is_interactive": node.is_interactive,
                    "focused": node.state.is_focused,
                    "disabled": node.state.is_disabled,
                })
            })
            .collect();

        info!("Query returned {} matching nodes", nodes_json.len());
        StepHandlerResult::success_with_data(json!({
            "count": nodes_json.len(),
            "nodes": nodes_json,
        }))
    }

    async fn action_ai_context(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let state = get_a11y_state!(context);
        let mgr = state.lock().await;

        if !mgr.is_connected() {
            return StepHandlerResult::failure("Not connected to accessibility source");
        }

        let max_elements = step
            .a11y_max_elements
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_ELEMENTS);
        let interactive_only = step.a11y_interactive_only.unwrap_or(false);

        let text = mgr.to_ai_context(max_elements, interactive_only).await;

        info!(
            "Generated AI context: {} chars, max_elements={}, interactive_only={}",
            text.len(),
            max_elements,
            interactive_only
        );
        StepHandlerResult::success_with_data(json!({
            "ai_context": text,
            "length": text.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_type() {
        let handler = NativeAccessibilityHandler;
        assert_eq!(handler.step_type(), "native_accessibility");
        assert_eq!(handler.display_name(), "Native Accessibility");
    }

    #[test]
    fn test_parse_target_desktop() {
        match NativeAccessibilityHandler::parse_target("Desktop") {
            ConnectionTarget::Desktop => {}
            other => panic!("Expected Desktop, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_target_pid() {
        match NativeAccessibilityHandler::parse_target("pid:1234") {
            ConnectionTarget::ProcessId(pid) => assert_eq!(pid, 1234),
            other => panic!("Expected ProcessId(1234), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_target_window_title() {
        match NativeAccessibilityHandler::parse_target("Notepad") {
            ConnectionTarget::WindowTitle(title) => assert_eq!(title, "Notepad"),
            other => panic!("Expected WindowTitle(Notepad), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_target_desktop_case_insensitive() {
        match NativeAccessibilityHandler::parse_target("desktop") {
            ConnectionTarget::Desktop => {}
            other => panic!("Expected Desktop, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_target_invalid_pid_falls_back() {
        match NativeAccessibilityHandler::parse_target("pid:notanumber") {
            ConnectionTarget::WindowTitle(title) => assert_eq!(title, "pid:notanumber"),
            other => panic!("Expected WindowTitle fallback, got {:?}", other),
        }
    }
}
