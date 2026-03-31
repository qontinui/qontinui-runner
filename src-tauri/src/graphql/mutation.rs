//! GraphQL mutation resolvers for qontinui-runner.
//!
//! Mutations for UI Bridge actions, navigation, form filling, circuit breaker control,
//! task run lifecycle, finding management, and workflow execution.
//! All mutations delegate to existing service functions preserving
//! circuit breaker, semaphore, and error classification.

use async_graphql::*;
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::ApiState;
use crate::mcp::ui_bridge;

use super::types::{ActionResult, GqlFindingStatus, GqlTaskRun};

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

    // ======================================================================
    // Task Run Lifecycle
    // ======================================================================

    /// Create a new task run.
    async fn create_task_run(
        &self,
        ctx: &Context<'_>,
        input: super::types::CreateTaskRunInput,
    ) -> Result<GqlTaskRun> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let id = uuid::Uuid::new_v4().to_string();

        let mut db_input =
            crate::database::CreateTaskRunInput::new(&id, &input.task_name)
                .with_task_type(&input.task_type);
        if let Some(ref p) = input.prompt {
            db_input = db_input.with_prompt(p);
        }
        if let Some(ref cid) = input.config_id {
            db_input = db_input.with_config_id(cid);
        }
        if let Some(ref wn) = input.workflow_name {
            db_input = db_input.with_workflow_name(wn);
        }
        if let Some(ref wid) = input.workflow_id {
            db_input = db_input.with_workflow_id(wid);
        }
        if let Some(ms) = input.max_sessions {
            db_input = db_input.with_max_sessions(ms as u32);
        }
        db_input = db_input.with_auto_continue(input.auto_continue);

        let port = state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);
        db_input.runner_port = Some(port);

        let run = 'pg: {
            match state.app_state.pg_db.create_task_run(&db_input).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for create_task_run, falling back to SQLite: {}", e),
            }
            return Err(Error::new("PG create_task_run failed (no SQLite fallback)"))
        };

        Ok(GqlTaskRun::from_db(run))
    }

    /// Stop a running task run (kills AI processes and marks as stopped).
    async fn stop_task_run(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let task_run = 'pg: {
            match state.app_state.pg_db.get_task_run(&id).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for get_task_run, falling back to SQLite: {}", e),
            }
            return Err(Error::new(format!("PG get_task_run failed for {}", id)))
        }
        .ok_or_else(|| Error::new(format!("Task run not found: {}", id)))?;

        if task_run.status != "running" {
            return Err(Error::new(format!(
                "Task is not running (status: {})",
                task_run.status
            )));
        }

        // Kill tracked AI processes
        let pids_to_kill: Vec<u32> = {
            let mut pids = crate::safe_lock::safe_lock_or_recover(
                &state.current_ai_pids,
                "current_ai_pids",
            );
            let copy = pids.clone();
            pids.clear();
            copy
        };
        for pid in &pids_to_kill {
            info!("GraphQL: Killing AI process PID {} for task {}", pid, id);
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
        }

        {
            let mut stopped = false;
            match state.app_state.pg_db.stop_task_run(&id, "User stopped").await {
                Ok(_) => stopped = true,
                Err(e) => tracing::warn!("PG failed for stop_task_run, falling back to SQLite: {}", e),
            }
            if !stopped {
                state.app_state.pg_db.stop_task_run(&id, "User stopped").await.map_err(|e| Error::new(e))?;
            }
        }

        // Release URL locks
        state.app_state.url_lock_manager.release_all(&id).await;

        // Release advisory file registry entries
        state.app_state.file_registry_manager.release_all(&id).await;

        // Broadcast update
        let broadcaster =
            crate::event_system::EventBroadcaster::new(state.app_handle.clone());
        broadcaster.task_run_update(&id, "stopped", None, None);

        Ok(true)
    }

    /// Pause a running task run.
    async fn pause_task_run(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state
            .app_state
            .pg_db
            .pause_task_run(&id)
            .await
            .map_err(|e| Error::new(e))
    }

    /// Unpause a paused task run.
    async fn unpause_task_run(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state
            .app_state
            .pg_db
            .unpause_task_run(&id)
            .await
            .map_err(|e| Error::new(e))
    }

    /// Delete a task run by ID.
    async fn delete_task_run(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state.app_state.pg_db.delete_task_run(&id)
            .await
            .map_err(|e| Error::new(e))
    }

    // ======================================================================
    // Finding Management
    // ======================================================================

    /// Update a finding's status (e.g., resolve, defer, mark won't-fix).
    async fn update_finding_status(
        &self,
        ctx: &Context<'_>,
        input: super::types::UpdateFindingStatusInput,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let domain_status = gql_status_to_domain(input.status);
        let resolution = input.resolution;
        let finding_id = input.finding_id;

        // PG-primary, SQLite fallback
        state.app_state.pg_db.update_finding_status(&finding_id, domain_status.as_str(), resolution.as_deref(), None)
            .await
            .map_err(|e| Error::new(e))?;

        Ok(true)
    }

    /// Set a user response on a finding that needs input.
    async fn respond_to_finding(
        &self,
        ctx: &Context<'_>,
        finding_id: String,
        response: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state.app_state.pg_db.set_finding_user_response(&finding_id, &response)
            .await
            .map_err(|e| Error::new(e))?;

        Ok(true)
    }

    // ======================================================================
    // Workflow Execution
    // ======================================================================

    /// Run a unified workflow by ID. Creates a task run record and returns it.
    /// Note: For full workflow execution (verification-agentic loop), use
    /// POST /unified-workflows/{id}/run which handles session management,
    /// command sanitization, and the full executor pipeline.
    async fn run_workflow(
        &self,
        ctx: &Context<'_>,
        workflow_id: String,
        prompt: Option<String>,
    ) -> Result<GqlTaskRun> {
        let state = ctx.data::<Arc<ApiState>>()?;
        // Fetch workflow to get its name
        let workflow = 'pg: {
            match state.app_state.pg_db.get_unified_workflow(&workflow_id).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for get_unified_workflow, falling back to SQLite: {}", e),
            }
            return Err(Error::new(format!("PG get_unified_workflow failed for {}", workflow_id)))
        }
        .ok_or_else(|| Error::new(format!("Workflow not found: {}", workflow_id)))?;

        // Create a task run for the workflow
        let task_id = uuid::Uuid::new_v4().to_string();
        let port = state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);

        let mut input = crate::database::CreateTaskRunInput::new(&task_id, &workflow.name)
            .with_task_type("task")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow_id)
            .with_workflow_type("unified")
            .with_max_sessions(workflow.max_iterations);
        input.runner_port = Some(port);
        if let Some(ref p) = prompt {
            input = input.with_prompt(p);
        }

        let run = 'pg: {
            match state.app_state.pg_db.create_task_run(&input).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for create_task_run in run_workflow, falling back to SQLite: {}", e),
            }
            return Err(Error::new("PG create_task_run failed in run_workflow (no SQLite fallback)"))
        };

        // Broadcast task creation (not "running" — actual execution is
        // triggered via POST /unified-workflows/{id}/run)
        let broadcaster =
            crate::event_system::EventBroadcaster::new(state.app_handle.clone());
        broadcaster.task_run_update(&task_id, &run.status, None, None);

        Ok(GqlTaskRun::from_db(run))
    }

    /// Delete a unified workflow by ID.
    async fn delete_workflow(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        'pg: {
            match state.app_state.pg_db.delete_unified_workflow(&id).await {
                Ok(r) => break 'pg Ok(r),
                Err(e) => tracing::warn!("PG failed for delete_workflow, falling back to SQLite: {}", e),
            }
            return Err(Error::new("PG delete_unified_workflow failed (no SQLite fallback)"))
        }
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

/// Convert GraphQL finding status enum to domain enum.
fn gql_status_to_domain(
    status: GqlFindingStatus,
) -> crate::findings::types::FindingStatus {
    use crate::findings::types::FindingStatus;
    match status {
        GqlFindingStatus::Detected => FindingStatus::Detected,
        GqlFindingStatus::InProgress => FindingStatus::InProgress,
        GqlFindingStatus::NeedsInput => FindingStatus::NeedsInput,
        GqlFindingStatus::Resolved => FindingStatus::Resolved,
        GqlFindingStatus::WontFix => FindingStatus::WontFix,
        GqlFindingStatus::Deferred => FindingStatus::Deferred,
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
    } else if msg.contains("not found") && msg.contains("window") {
        UiBridgeErrorCode::WindowNotFound
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
    } else if msg.contains("assertion") && msg.contains("failed") {
        UiBridgeErrorCode::AssertionFailed
    } else if msg.contains("unknown") && msg.contains("assertion") {
        UiBridgeErrorCode::UnknownAssertionType
    } else {
        UiBridgeErrorCode::InternalError
    }
}
