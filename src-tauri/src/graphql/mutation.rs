//! GraphQL mutation resolvers for qontinui-runner.
//!
//! Mutations for UI Bridge actions, navigation, form filling, circuit breaker control.
//! All mutations delegate to existing `ui_bridge_request_sync()` preserving
//! circuit breaker, semaphore, and error classification.

use async_graphql::*;
use std::sync::Arc;

use crate::mcp::types::ApiState;
use crate::mcp::ui_bridge;

use super::types::ActionResult;

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    // ======================================================================
    // Element Actions
    // ======================================================================

    /// Execute an action on a UI element (click, type, scroll, etc.).
    async fn ui_bridge_execute_action(
        &self,
        ctx: &Context<'_>,
        element_id: String,
        action: String,
        params: Option<Json<serde_json::Value>>,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let payload = serde_json::json!({
            "elementId": element_id,
            "action": action,
            "params": params.map(|p| p.0).unwrap_or(serde_json::json!({})),
        });
        bridge_mutation(state, "execute_action", payload).await
    }

    // ======================================================================
    // Page Navigation
    // ======================================================================

    /// Navigate to a URL.
    async fn ui_bridge_page_navigate(
        &self,
        ctx: &Context<'_>,
        url: String,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "page_navigate", serde_json::json!({ "url": url })).await
    }

    /// Refresh the current page.
    async fn ui_bridge_page_refresh(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = false)] hard: bool,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let command = if hard {
            "page_hard_refresh"
        } else {
            "page_refresh"
        };
        bridge_mutation(state, command, serde_json::json!({})).await
    }

    /// Navigate back in browser history.
    async fn ui_bridge_page_back(&self, ctx: &Context<'_>) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "page_go_back", serde_json::json!({})).await
    }

    /// Navigate forward in browser history.
    async fn ui_bridge_page_forward(&self, ctx: &Context<'_>) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "page_go_forward", serde_json::json!({})).await
    }

    // ======================================================================
    // Form Interaction
    // ======================================================================

    /// Fill a form with the provided values.
    async fn ui_bridge_fill_form(
        &self,
        ctx: &Context<'_>,
        form_id: Option<String>,
        values: Json<serde_json::Value>,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let payload = serde_json::json!({
            "formId": form_id,
            "values": values.0,
        });
        bridge_mutation(state, "fill_form", payload).await
    }

    // ======================================================================
    // JavaScript Evaluation
    // ======================================================================

    /// Execute a CSS selector query in the browser.
    async fn ui_bridge_query_selector(
        &self,
        ctx: &Context<'_>,
        selector: String,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        ui_bridge::ui_bridge_request_sync(
            state,
            "query_selector",
            serde_json::json!({ "selector": selector }),
        )
        .await
        .map(Json)
        .map_err(|e| Error::new(e))
    }

    /// Evaluate a JavaScript expression in the browser.
    async fn ui_bridge_evaluate(
        &self,
        ctx: &Context<'_>,
        expression: String,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        ui_bridge::ui_bridge_request_sync(
            state,
            "page_evaluate",
            serde_json::json!({ "expression": expression }),
        )
        .await
        .map(Json)
        .map_err(|e| Error::new(e))
    }

    // ======================================================================
    // Undo/Redo
    // ======================================================================

    /// Trigger undo in the connected browser.
    async fn ui_bridge_undo(&self, ctx: &Context<'_>) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "undo", serde_json::json!({})).await
    }

    /// Trigger redo in the connected browser.
    async fn ui_bridge_redo(&self, ctx: &Context<'_>) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "redo", serde_json::json!({})).await
    }

    // ======================================================================
    // Console Management
    // ======================================================================

    /// Clear captured console errors.
    async fn ui_bridge_clear_console_errors(&self, ctx: &Context<'_>) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(state, "clear_console_errors", serde_json::json!({})).await
    }

    // ======================================================================
    // Circuit Breaker Control
    // ======================================================================

    /// Reset the UI Bridge circuit breaker to Closed state.
    async fn ui_bridge_reset_circuit_breaker(&self, ctx: &Context<'_>) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state.ui_bridge_circuit_breaker.reset().await;
        Ok(true)
    }

    // ======================================================================
    // Generic Command
    // ======================================================================

    /// Execute any UI Bridge command with raw JSON params.
    async fn ui_bridge_raw_command(
        &self,
        ctx: &Context<'_>,
        command: String,
        params: Option<Json<serde_json::Value>>,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_mutation(
            state,
            &command,
            params.map(|p| p.0).unwrap_or(serde_json::json!({})),
        )
        .await
    }

    /// Simple ping mutation for connectivity testing.
    async fn ping(&self) -> Result<String> {
        Ok("pong".to_string())
    }
}

// ==========================================================================
// Helpers
// ==========================================================================

/// Delegate a command to the UI Bridge and wrap as ActionResult.
async fn bridge_mutation(
    state: &Arc<ApiState>,
    command: &str,
    params: serde_json::Value,
) -> Result<ActionResult> {
    let start = std::time::Instant::now();
    match ui_bridge::ui_bridge_request_sync(state, command, params).await {
        Ok(data) => Ok(ActionResult {
            success: true,
            data: Some(Json(data)),
            error: None,
            duration_ms: start.elapsed().as_millis().to_string(),
        }),
        Err(e) => {
            let error_detail = super::types::UiBridgeErrorDetail {
                code: classify_error(&e),
                message: e,
                recovery: None,
                context: None,
            };
            Ok(ActionResult {
                success: false,
                data: None,
                error: Some(error_detail),
                duration_ms: start.elapsed().as_millis().to_string(),
            })
        }
    }
}

/// Classify an error message into a typed error code.
fn classify_error(error_msg: &str) -> super::types::UiBridgeErrorCode {
    use super::types::UiBridgeErrorCode;
    let msg = error_msg.to_lowercase();
    if msg.contains("timed out") || msg.contains("timeout") {
        UiBridgeErrorCode::Timeout
    } else if msg.contains("circuit breaker") {
        UiBridgeErrorCode::CircuitBreakerOpen
    } else if msg.contains("concurrency") || msg.contains("semaphore") {
        UiBridgeErrorCode::ConcurrencyLimitReached
    } else if msg.contains("unresponsive") || msg.contains("not responsive") {
        UiBridgeErrorCode::FrontendUnresponsive
    } else if msg.contains("not found") && msg.contains("element") {
        UiBridgeErrorCode::ElementNotFound
    } else if msg.contains("not visible") {
        UiBridgeErrorCode::ElementNotVisible
    } else if msg.contains("not enabled") || msg.contains("disabled") {
        UiBridgeErrorCode::ElementNotEnabled
    } else if msg.contains("stale") {
        UiBridgeErrorCode::ElementStale
    } else if msg.contains("action failed") {
        UiBridgeErrorCode::ActionFailed
    } else {
        UiBridgeErrorCode::InternalError
    }
}
