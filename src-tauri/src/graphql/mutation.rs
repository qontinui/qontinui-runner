//! GraphQL mutation resolvers for qontinui-runner.
//!
//! Mutations for UI Bridge actions, navigation, form filling, circuit breaker control,
//! task run lifecycle, finding management, and workflow execution.
//! All mutations delegate to existing service functions preserving
//! circuit breaker, semaphore, and error classification.

use async_graphql::*;
use std::sync::Arc;
use tracing::info;

use crate::findings::FindingStatusExt;
use crate::mcp::types::ApiState;
use crate::mcp::ui_bridge;
use crate::unified_workflows::UnifiedWorkflowExt;

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
        // `params` must be nested INSIDE the action envelope. It used to be a
        // top-level sibling of a bare-string `action`, which the frontend
        // handler never reads — so every GraphQL element action ran with no
        // parameters at all (a `type` mutation typed nothing) while still
        // reporting success. See `request::element_action_payload`.
        let payload = ui_bridge::request::element_action_payload(
            &element_id,
            serde_json::json!({
                "action": action,
                "params": params.map(|p| p.0).unwrap_or(serde_json::json!({})),
            }),
        );
        bridge_mutation(state, "execute_action", payload).await
    }

    // ======================================================================
    // Page Navigation
    // ======================================================================

    /// Navigate to a URL. Unrouted targets are rejected, not reported as success.
    async fn ui_bridge_page_navigate(
        &self,
        ctx: &Context<'_>,
        url: String,
    ) -> Result<ActionResult> {
        let state = ctx.data::<Arc<ApiState>>()?;
        // Gated on the same `PAGE_TO_TAB`-derived route table the REST
        // `page/navigate` uses. Without it this mutation reproduces the defect
        // that route just closed — reporting success for a page the app never
        // navigated to, with the snapshot's `route` echoing the request back as
        // its own evidence.
        //
        // Surfaced as a GraphQL error rather than an `ActionResult` with a
        // code: an unrouted target is a bad ARGUMENT, settled before any action
        // runs, and `UiBridgeErrorCode` has no `InvalidRequest` variant to
        // widen the published schema with for it. (The doc comment above stays
        // one line for the same reason — it is the SDL description.)
        //
        // Iteration 21: the gate used to read `if trimmed.starts_with('/')`,
        // so an ABSOLUTE same-origin URL skipped it and the frontend then
        // discarded it while this mutation answered success.
        // `resolve_navigate_target` normalizes first — one shared resolver for
        // this mutation, the REST control route and the SDK fallback — and the
        // NORMALIZED url is what is forwarded, since `usePageEvents.ts` acts
        // on relative paths only.
        let trimmed = url.trim();
        let normalized = match crate::mcp::ui_bridge::page::resolve_navigate_target(trimmed) {
            Ok((normalized, _page)) => normalized,
            Err(crate::mcp::ui_bridge::page::NavigateRejection::NotNavigable) => {
                return Err(async_graphql::Error::new(format!(
                    "page/navigate: `{trimmed}` is neither a relative path nor a same-origin \
                     (localhost / 127.0.0.1) URL, so the runner cannot navigate to it."
                )));
            }
            Err(crate::mcp::ui_bridge::page::NavigateRejection::UnroutedPage(rejected)) => {
                return Err(async_graphql::Error::new(format!(
                    "page/navigate: `{trimmed}` resolves to page `{rejected}`, which the runner \
                     has no route for. See PAGE_TO_TAB in \
                     src/components/app/useAppNavigation.ts for the navigable pages."
                )));
            }
        };
        bridge_mutation(
            state,
            "page_navigate",
            serde_json::json!({ "url": normalized }),
        )
        .await
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

        let mut db_input = crate::database::CreateTaskRunInput::new(&id, &input.task_name)
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
                Err(e) => tracing::warn!("PG create_task_run failed: {}", e),
            }
            return Err(Error::new("PG create_task_run failed"));
        };

        Ok(GqlTaskRun::from_db(run))
    }

    /// Stop a running task run (kills AI processes and marks as stopped).
    async fn stop_task_run(&self, ctx: &Context<'_>, id: String) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let task_run = 'pg: {
            match state.app_state.pg_db.get_task_run(&id).await {
                Ok(r) => break 'pg r,
                Err(e) => {
                    tracing::warn!("PG failed for get_task_run, falling back to SQLite: {}", e)
                }
            }
            return Err(Error::new(format!("PG get_task_run failed for {}", id)));
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
            let mut pids =
                crate::safe_lock::safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
            let copy = pids.clone();
            pids.clear();
            copy
        };
        for pid in &pids_to_kill {
            info!("GraphQL: Killing AI process PID {} for task {}", pid, id);
            #[cfg(target_os = "windows")]
            {
                let _ = crate::process_helpers::no_window("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                // console-ok: the non-Windows arm; the Windows one above is suppressed.
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
        }

        {
            let mut stopped = false;
            match state
                .app_state
                .pg_db
                .stop_task_run(&id, "User stopped")
                .await
            {
                Ok(_) => stopped = true,
                Err(e) => {
                    tracing::warn!("PG failed for stop_task_run, falling back to SQLite: {}", e)
                }
            }
            if !stopped {
                state
                    .app_state
                    .pg_db
                    .stop_task_run(&id, "User stopped")
                    .await
                    .map_err(|e| Error::new(e))?;
            }
        }

        // Expire any waiting breakpoint snapshots for this task (cleanup)
        let _ = state.app_state.pg_db.expire_breakpoint_snapshots(&id).await;

        // Release URL locks
        state.app_state.url_lock_manager.release_all(&id).await;

        // Release advisory file registry entries and exclusive file locks
        state.app_state.file_registry_manager.release_all(&id).await;
        let released_paths = state.app_state.file_lock_manager.release_all(&id).await;
        for released_path in &released_paths {
            use tauri::Emitter;
            let payload = serde_json::json!({
                "type": "file-lock-released",
                "file_path": released_path,
                "task_run_id": id,
                "holder_name": id,
            });
            let _ = state.app_handle.emit("file-lock-released", &payload);
        }

        // Broadcast update
        let broadcaster = crate::event_system::EventBroadcaster::new(state.app_handle.clone());
        broadcaster.task_run_update(&id, "stopped", None, None);

        Ok(true)
    }

    /// Pause a running task run.
    async fn pause_task_run(&self, ctx: &Context<'_>, id: String) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state
            .app_state
            .pg_db
            .pause_task_run(&id)
            .await
            .map_err(|e| Error::new(e))
    }

    /// Unpause a paused task run.
    async fn unpause_task_run(&self, ctx: &Context<'_>, id: String) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state
            .app_state
            .pg_db
            .unpause_task_run(&id)
            .await
            .map_err(|e| Error::new(e))
    }

    /// Delete a task run by ID.
    async fn delete_task_run(&self, ctx: &Context<'_>, id: String) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state
            .app_state
            .pg_db
            .delete_task_run(&id)
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

        state
            .app_state
            .pg_db
            .update_finding_status(
                &finding_id,
                domain_status.as_str(),
                resolution.as_deref(),
                None,
            )
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
        state
            .app_state
            .pg_db
            .set_finding_user_response(&finding_id, &response)
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
            match state
                .app_state
                .pg_db
                .get_unified_workflow(&workflow_id)
                .await
            {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!(
                    "PG failed for get_unified_workflow, falling back to SQLite: {}",
                    e
                ),
            }
            return Err(Error::new(format!(
                "PG get_unified_workflow failed for {}",
                workflow_id
            )));
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
            .with_max_sessions(workflow.iter_cap());
        input.runner_port = Some(port);
        if let Some(ref p) = prompt {
            input = input.with_prompt(p);
        }

        let run = 'pg: {
            match state.app_state.pg_db.create_task_run(&input).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG create_task_run failed in run_workflow: {}", e),
            }
            return Err(Error::new("PG create_task_run failed in run_workflow"));
        };

        // Broadcast task creation (not "running" — actual execution is
        // triggered via POST /unified-workflows/{id}/run)
        let broadcaster = crate::event_system::EventBroadcaster::new(state.app_handle.clone());
        broadcaster.task_run_update(&task_id, &run.status, None, None);

        Ok(GqlTaskRun::from_db(run))
    }

    /// Delete a unified workflow by ID.
    async fn delete_workflow(&self, ctx: &Context<'_>, id: String) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        'pg: {
            match state.app_state.pg_db.delete_unified_workflow(&id).await {
                Ok(r) => break 'pg Ok(r),
                Err(e) => tracing::warn!("PG delete_unified_workflow failed: {}", e),
            }
            return Err(Error::new("PG delete_unified_workflow failed"));
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
    let result = ui_bridge::ui_bridge_request_sync(state, command, params).await;
    Ok(action_result_for(
        result,
        start.elapsed().as_millis().to_string(),
    ))
}

/// Map a UI Bridge IPC result onto an [`ActionResult`] with the SAME verdict
/// and classification the HTTP surface gives it (`request::wrap_ipc_result`).
///
/// A frontend refusal does not arrive as `Err`: the IPC round-trip succeeds and
/// carries `{success: false, error, code}` in `Ok`. Only transport failures
/// (timeout, readiness, circuit breaker, concurrency) are `Err`. So both arms
/// can fail:
///
/// - `Ok` with `success == false` → failure; the handler's own typed `code`
///   field wins (`typed_frontend_code_by_name`), else the error message is
///   classified. The payload is kept in `data` for context.
/// - `Ok` otherwise (`success` true or absent) → success.
/// - `Err` → failure, classified from the transport message.
fn action_result_for(
    result: std::result::Result<serde_json::Value, String>,
    duration_ms: String,
) -> ActionResult {
    match result {
        Ok(data) if data.get("success").and_then(|v| v.as_bool()) == Some(false) => {
            let error_msg = data
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "UI bridge call failed".to_string());
            let classified = data
                .get("code")
                .and_then(|v| v.as_str())
                .and_then(|name| ui_bridge::types::typed_frontend_code_by_name(name, &error_msg))
                .unwrap_or_else(|| ui_bridge::classify_transport_error(&error_msg));
            ActionResult {
                success: false,
                data: Some(Json(data)),
                error: Some(error_detail_from(classified, error_msg)),
                duration_ms,
            }
        }
        Ok(data) => ActionResult {
            success: true,
            data: Some(Json(data)),
            error: None,
            duration_ms,
        },
        Err(e) => ActionResult {
            success: false,
            data: None,
            error: Some(error_detail_from(
                ui_bridge::classify_transport_error(&e),
                e,
            )),
            duration_ms,
        },
    }
}

/// Convert GraphQL finding status enum to domain enum.
fn gql_status_to_domain(status: GqlFindingStatus) -> crate::findings::types::FindingStatus {
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

/// Build the GraphQL error detail from a canonical classification, so a
/// GraphQL client sees the identical code, recovery hint and context an
/// MCP/HTTP caller would. A private substring classifier stood here until
/// 2026-09-23; it could only ever emit the 13 codes the old hand-copied
/// GraphQL enum carried.
fn error_detail_from(
    classified: ui_bridge::UiBridgeError,
    message: String,
) -> super::types::UiBridgeErrorDetail {
    super::types::UiBridgeErrorDetail {
        code: classified.code,
        message,
        recovery: classified.recovery.as_ref().map(recovery_hint_wire),
        context: classified.context.map(Json),
    }
}

/// The recovery hint's serde wire spelling as a string: `"RESNAPSHOT"` for a
/// unit hint, the compact JSON (`{"RETRY_AFTER_MS":1000}`) for a data-carrying
/// one — the same value an HTTP caller reads under `recovery`.
fn recovery_hint_wire(hint: &ui_bridge::RecoveryHint) -> String {
    match serde_json::to_value(hint) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(e) => format!("UNSERIALIZABLE_RECOVERY_HINT: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::types::UiBridgeErrorCode;
    use serde_json::json;

    fn run(result: std::result::Result<serde_json::Value, String>) -> ActionResult {
        action_result_for(result, "0".into())
    }

    /// A frontend refusal arrives as `Ok({success:false, error, code})`, not
    /// `Err`. It must be a failure carrying the handler's own typed code — a
    /// code the retired private classifier could not even name.
    #[test]
    fn frontend_refusal_in_ok_is_a_failure_with_its_typed_code() {
        let out = run(Ok(json!({
            "success": false,
            "error": "SEND_KEYS_INVALID: 'modifiers' must be an array",
            "code": "SEND_KEYS_INVALID",
        })));
        assert!(!out.success);
        let detail = out.error.expect("failure carries a detail");
        assert_eq!(detail.code, UiBridgeErrorCode::SendKeysInvalid);
        assert_eq!(detail.recovery.as_deref(), Some("FIX_REQUEST"));
        assert_eq!(
            detail.message,
            "SEND_KEYS_INVALID: 'modifiers' must be an array"
        );
        assert!(
            out.data.is_some(),
            "the refusal payload is kept for context"
        );
    }

    /// With no `code` field the message is classified — including the
    /// `"<CODE>: "` prefix contract.
    #[test]
    fn frontend_refusal_without_code_field_is_classified_from_message() {
        let out = run(Ok(json!({
            "success": false,
            "error": "TERMINAL_NO_MOUNTED_VIEW: terminal has no mounted view",
        })));
        assert!(!out.success);
        assert_eq!(
            out.error.unwrap().code,
            UiBridgeErrorCode::TerminalNoMountedView
        );
    }

    /// Positive controls: `success: true` and absent `success` stay success.
    #[test]
    fn healthy_ok_stays_success() {
        for data in [
            json!({ "success": true, "clicked": true }),
            json!({ "clicked": true }),
        ] {
            let out = run(Ok(data.clone()));
            assert!(out.success, "{data} is a healthy response");
            assert!(out.error.is_none());
            assert_eq!(out.data.map(|j| j.0), Some(data));
        }
    }

    #[test]
    fn transport_timeout_renders_data_carrying_hint_as_json() {
        let out = run(Err("UI Bridge request timed out after 5000ms".into()));
        assert!(!out.success);
        let detail = out.error.unwrap();
        assert_eq!(detail.code, UiBridgeErrorCode::Timeout);
        assert_eq!(
            detail.recovery.as_deref(),
            Some(r#"{"RETRY_AFTER_MS":1000}"#)
        );
    }

    /// The exact string `request.rs` returns when the permit wait times out.
    #[test]
    fn transport_concurrency_limit_is_classified() {
        let out = run(Err(
            "UI Bridge concurrency limit reached (timeout acquiring permit)".into(),
        ));
        assert_eq!(
            out.error.unwrap().code,
            UiBridgeErrorCode::ConcurrencyLimitReached
        );
    }

    /// The readiness gate returns the SERIALIZED `gather_readiness_diagnostics`
    /// body (shape copied from `request.rs`). It must classify as
    /// `FRONTEND_NOT_READY` with the diagnostics as context — before this
    /// change the shared classifier only matched prose this body never
    /// contains, and answered `INTERNAL_ERROR` on HTTP as well.
    #[test]
    fn transport_readiness_diagnostics_json_is_frontend_not_ready() {
        let body = json!({
            "error": "frontend_not_ready",
            "diagnostics": { "last_pong_age_ms": null, "sdk_connected": false, "hint": "x" }
        });
        let out = run(Err(serde_json::to_string(&body).unwrap()));
        let detail = out.error.unwrap();
        assert_eq!(detail.code, UiBridgeErrorCode::FrontendNotReady);
        assert_eq!(detail.context.map(|j| j.0), Some(body));
        assert_eq!(
            detail.recovery.as_deref(),
            Some(r#"{"RETRY_AFTER_MS":2000}"#)
        );
    }
}
