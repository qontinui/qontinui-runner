//! GraphQL query resolvers for qontinui-runner.
//!
//! All UI Bridge query resolvers delegate to the existing `ui_bridge_request_sync()`
//! function, preserving circuit breaker, semaphore, dedup, and error classification.

use async_graphql::*;
use std::sync::Arc;

use crate::mcp::types::ApiState;
use crate::mcp::ui_bridge::{self, CircuitBreakerState as CbState};

use super::types::{CircuitBreakerState, SdkConnectionInfo, UiBridgeHealth};

pub struct QueryRoot;

/// Convert the internal CircuitBreakerState to the GraphQL enum.
fn convert_cb_state(state: &CbState) -> CircuitBreakerState {
    match state {
        CbState::Open => CircuitBreakerState::Open,
        CbState::HalfOpen => CircuitBreakerState::HalfOpen,
        CbState::Closed => CircuitBreakerState::Closed,
    }
}

#[Object]
impl QueryRoot {
    // ======================================================================
    // Health & Diagnostics
    // ======================================================================

    /// Get the current health status of the UI Bridge relay.
    async fn ui_bridge_health(&self, ctx: &Context<'_>) -> Result<UiBridgeHealth> {
        let state = ctx.data::<Arc<ApiState>>()?;
        Ok(build_health_snapshot(state).await)
    }

    /// Get full UI Bridge diagnostics as raw JSON.
    async fn ui_bridge_diagnostics(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let uptime_secs = state.started_at.elapsed().as_secs();
        let last_pong = state
            .ui_bridge_last_pong
            .load(std::sync::atomic::Ordering::Relaxed);
        let now_ms = now_epoch_ms();
        let pong_age_ms = if last_pong > 0 {
            now_ms - last_pong
        } else {
            0
        };
        let pending_count = pending_count_with_timeout(state).await;
        let cb_state = state.ui_bridge_circuit_breaker.get_state().await;

        Ok(Json(serde_json::json!({
            "responsive": last_pong > 0 && pong_age_ms < 15000,
            "lastHeartbeat": last_pong,
            "heartbeatAgeMs": pong_age_ms,
            "uptimeSeconds": uptime_secs,
            "pendingRequests": pending_count,
            "circuitBreaker": format!("{:?}", cb_state),
            "semaphoreAvailable": state.ui_bridge_semaphore.available_permits(),
        })))
    }

    // ======================================================================
    // UI Bridge Element Queries (delegate to ui_bridge_request_sync)
    // ======================================================================

    /// Get all registered UI elements from the connected browser.
    async fn ui_bridge_elements(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_elements", serde_json::json!({})).await
    }

    /// Get a specific element by ID.
    async fn ui_bridge_element(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_element", serde_json::json!({ "elementId": id })).await
    }

    /// Get a full DOM snapshot from the connected browser.
    async fn ui_bridge_snapshot(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_snapshot", serde_json::json!({})).await
    }

    /// Get all registered React/Vue/etc. components.
    async fn ui_bridge_components(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_components", serde_json::json!({})).await
    }

    /// Get a specific component by ID.
    async fn ui_bridge_component(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(
            state,
            "get_component",
            serde_json::json!({ "componentId": id }),
        )
        .await
    }

    /// List all open windows/tabs.
    async fn ui_bridge_windows(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "list_windows", serde_json::json!({})).await
    }

    /// Get console errors from the connected browser.
    async fn ui_bridge_console_errors(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_console_errors", serde_json::json!({})).await
    }

    /// Get network requests captured by the browser.
    async fn ui_bridge_network_requests(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_network_requests", serde_json::json!({})).await
    }

    /// Get all forms on the current page.
    async fn ui_bridge_forms(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_forms", serde_json::json!({})).await
    }

    /// Get the idle/busy status of the browser.
    async fn ui_bridge_idle_status(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_idle_status", serde_json::json!({})).await
    }

    /// Get keyboard shortcuts registered in the connected app.
    async fn ui_bridge_keyboard_shortcuts(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_keyboard_shortcuts", serde_json::json!({})).await
    }

    /// Get the render log (in-memory circular buffer of render operations).
    async fn ui_bridge_render_log(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let log = state.ui_bridge_render_log.lock().await;
        let entries: Vec<_> = if let Some(limit) = limit {
            log.iter()
                .rev()
                .take(limit.max(0) as usize)
                .cloned()
                .collect()
        } else {
            log.clone()
        };
        Ok(Json(serde_json::json!(entries)))
    }

    /// Get undo/redo state from the connected browser.
    async fn ui_bridge_undo_state(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_undo_state", serde_json::json!({})).await
    }

    /// Get page specs registered via the UI Bridge SDK.
    async fn ui_bridge_specs(&self, ctx: &Context<'_>) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_specs", serde_json::json!({})).await
    }

    /// Get a specific page spec by ID.
    async fn ui_bridge_spec(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(state, "get_spec", serde_json::json!({ "specId": id })).await
    }

    /// Generic UI Bridge query — send any command and get raw JSON back.
    /// Use this for commands not yet covered by typed resolvers.
    async fn ui_bridge_raw_query(
        &self,
        ctx: &Context<'_>,
        command: String,
        params: Option<Json<serde_json::Value>>,
    ) -> Result<Json<serde_json::Value>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        bridge_query(
            state,
            &command,
            params.map(|p| p.0).unwrap_or(serde_json::json!({})),
        )
        .await
    }

    // ======================================================================
    // SDK Connection Queries
    // ======================================================================

    /// List all connected SDK app connections.
    async fn sdk_connections(&self, ctx: &Context<'_>) -> Result<Vec<SdkConnectionInfo>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let manager = state.sdk_connection.lock().await;
        let mut connections = Vec::new();
        let active_url = manager.active_url.clone();

        for (_key, conn) in &manager.connections {
            connections.push(SdkConnectionInfo {
                app_id: conn.app_info.app_id.clone(),
                app_name: conn.app_info.app_name.clone(),
                url: conn.app_url.clone(),
                responsive: active_url.as_deref() == Some(&conn.app_url)
                    && manager.active_responsive.unwrap_or(false),
                is_active: active_url.as_deref() == Some(&conn.app_url),
            });
        }
        Ok(connections)
    }

    // ======================================================================
    // Task Run Queries
    // ======================================================================

    /// List recent task runs with optional filters.
    async fn task_runs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: i32,
        workflow_type: Option<String>,
    ) -> Result<Vec<super::types::GqlTaskRun>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let port = state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);

        let runs = 'pg: {
            match state.app_state.pg_db.get_recent_task_runs_filtered(limit as u32, workflow_type.as_deref(), Some(port)).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for task_runs, falling back to SQLite: {}", e),
            }
            let db = state.app_state.checkpoint_db.clone();
            tokio::task::spawn_blocking(move || {
                db.get_recent_task_runs_filtered(limit as u32, workflow_type.as_deref(), Some(port))
            })
            .await
            .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
            .map_err(|e| Error::new(e))?
        };

        Ok(runs.into_iter().map(super::types::GqlTaskRun::from_db).collect())
    }

    /// Get a single task run by ID.
    async fn task_run(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Option<super::types::GqlTaskRun>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let run = 'pg: {
            match state.app_state.pg_db.get_task_run(&id).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for task_run, falling back to SQLite: {}", e),
            }
            let db = state.app_state.checkpoint_db.clone();
            tokio::task::spawn_blocking(move || db.get_task_run(&id))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?
        };

        Ok(run.map(super::types::GqlTaskRun::from_db))
    }

    /// Get task run output with optional offset/limit for pagination.
    /// Defaults to last 10000 characters (tail) if no offset specified.
    async fn task_run_output(
        &self,
        ctx: &Context<'_>,
        id: String,
        #[graphql(default = 0)] offset: i32,
        #[graphql(default = 10000)] limit: i32,
    ) -> Result<super::types::GqlTaskRunOutput> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let full_output = 'pg: {
            match state.app_state.pg_db.get_task_output(&id).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for task_run_output, falling back to SQLite: {}", e),
            }
            let db = state.app_state.checkpoint_db.clone();
            let id_clone = id.clone();
            tokio::task::spawn_blocking(move || db.get_full_task_output(&id_clone))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?
        };

        let total_length = full_output.len() as i32;
        let offset = offset.max(0) as usize;
        let limit = limit.max(0) as usize;

        let content = if offset < full_output.len() {
            let end = (offset + limit).min(full_output.len());
            full_output[offset..end].to_string()
        } else {
            String::new()
        };

        let has_more = (offset + limit) < full_output.len();

        Ok(super::types::GqlTaskRunOutput {
            task_run_id: id,
            content,
            total_length,
            offset: offset as i32,
            has_more,
        })
    }

    /// List only currently running task runs.
    async fn running_task_runs(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<super::types::GqlTaskRun>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let db = state.app_state.checkpoint_db.clone();
        let port = state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);

        let runs = tokio::task::spawn_blocking(move || db.get_running_task_runs(Some(port)))
            .await
            .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
            .map_err(|e| Error::new(e))?;

        Ok(runs.into_iter().map(super::types::GqlTaskRun::from_db).collect())
    }

    /// Get child task runs of a parent task.
    async fn child_task_runs(
        &self,
        ctx: &Context<'_>,
        parent_task_run_id: String,
    ) -> Result<Vec<super::types::GqlTaskRun>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let db = state.app_state.checkpoint_db.clone();
        let runs =
            tokio::task::spawn_blocking(move || db.get_child_task_runs(&parent_task_run_id))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?;

        Ok(runs.into_iter().map(super::types::GqlTaskRun::from_db).collect())
    }

    // ======================================================================
    // Workflow Queries
    // ======================================================================

    /// List all unified workflows (summary view).
    async fn workflows(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<super::types::GqlWorkflowSummary>> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let workflows = 'pg: {
            match state.app_state.pg_db.list_unified_workflows().await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for workflows, falling back to SQLite: {}", e),
            }
            let db = state.app_state.checkpoint_db.clone();
            tokio::task::spawn_blocking(move || db.list_unified_workflows())
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?
        };

        Ok(workflows
            .into_iter()
            .map(|w| super::types::GqlWorkflowSummary {
                stage_count: w.stages.len() as i32,
                id: w.id,
                name: w.name,
                description: w.description,
                category: w.category,
                tags: w.tags,
                max_iterations: w.max_iterations as i32,
                timeout_seconds: w.timeout_seconds.map(|v| v as i64),
                provider: w.provider,
                model: w.model,
                is_favorite: w.is_favorite,
                multi_agent_mode: w.multi_agent_mode,
                workflow_architecture: w.workflow_architecture.map(|a| format!("{:?}", a)),
                created_at: w.created_at,
                updated_at: w.updated_at,
            })
            .collect())
    }

    /// Get a full workflow definition by ID.
    async fn workflow(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Option<super::types::GqlWorkflow>> {
        let state = ctx.data::<Arc<ApiState>>()?;

        let workflow = 'pg: {
            match state.app_state.pg_db.get_unified_workflow(&id).await {
                Ok(r) => break 'pg r,
                Err(e) => tracing::warn!("PG failed for workflow, falling back to SQLite: {}", e),
            }
            let db = state.app_state.checkpoint_db.clone();
            tokio::task::spawn_blocking(move || db.get_unified_workflow(&id))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?
        };

        Ok(workflow.map(|w| {
            super::types::GqlWorkflow {
                id: w.id,
                name: w.name,
                description: w.description,
                category: w.category,
                tags: w.tags,
                setup_steps: Json(serde_json::to_value(&w.setup_steps).unwrap_or_default()),
                verification_steps: Json(
                    serde_json::to_value(&w.verification_steps).unwrap_or_default(),
                ),
                agentic_steps: Json(
                    serde_json::to_value(&w.agentic_steps).unwrap_or_default(),
                ),
                completion_steps: Json(
                    serde_json::to_value(&w.completion_steps).unwrap_or_default(),
                ),
                max_iterations: w.max_iterations as i32,
                timeout_seconds: w.timeout_seconds.map(|v| v as i64),
                provider: w.provider,
                model: w.model,
                is_favorite: w.is_favorite,
                multi_agent_mode: w.multi_agent_mode,
                reflection_mode: w.reflection_mode,
                approval_gate: w.approval_gate,
                stop_on_failure: w.stop_on_failure,
                use_worktree: w.use_worktree,
                strict_cwd: w.strict_cwd,
                enforce_token_budget: w.enforce_token_budget,
                workflow_architecture: w.workflow_architecture.map(|a| format!("{:?}", a)),
                stages: Json(serde_json::to_value(&w.stages).unwrap_or_default()),
                created_at: w.created_at,
                updated_at: w.updated_at,
            }
        }))
    }

    // ======================================================================
    // Finding Queries
    // ======================================================================

    /// Get findings for a specific task run.
    async fn findings(
        &self,
        ctx: &Context<'_>,
        task_run_id: String,
    ) -> Result<Vec<super::types::GqlFinding>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let db = state.app_state.checkpoint_db.clone();

        let findings =
            tokio::task::spawn_blocking(move || db.get_findings_for_task(&task_run_id))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?;

        Ok(findings
            .into_iter()
            .map(super::types::GqlFinding::from_domain)
            .collect())
    }

    /// Get finding summary statistics for a task run.
    async fn finding_summary(
        &self,
        ctx: &Context<'_>,
        task_run_id: String,
    ) -> Result<super::types::GqlFindingSummary> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let db = state.app_state.checkpoint_db.clone();

        let summary =
            tokio::task::spawn_blocking(move || db.get_finding_summary(&task_run_id))
                .await
                .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?
                .map_err(|e| Error::new(e))?;

        Ok(super::types::GqlFindingSummary {
            task_run_id: summary.task_run_id,
            total: summary.total as i32,
            by_category: Json(serde_json::to_value(&summary.by_category).unwrap_or_default()),
            by_severity: Json(serde_json::to_value(&summary.by_severity).unwrap_or_default()),
            by_status: Json(serde_json::to_value(&summary.by_status).unwrap_or_default()),
            needs_input_count: summary.needs_input_count as i32,
            resolved_count: summary.resolved_count as i32,
            outstanding_count: summary.outstanding_count as i32,
        })
    }

    // ======================================================================
    // Runner Status Queries
    // ======================================================================

    /// Get the current runner status.
    async fn runner_status(
        &self,
        ctx: &Context<'_>,
    ) -> Result<super::types::GqlRunnerStatus> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let app_state = state.app_state.clone();

        let (executor_running, executor_state, config_loaded, config_path) =
            tokio::task::spawn_blocking(move || {
                let (executor_running, executor_state) =
                    match crate::executor::with_default_bridge(&app_state, |bridge| {
                        (bridge.is_running(), bridge.get_state().name().to_string())
                    }) {
                        Ok(result) => result,
                        Err(_) => (false, "not_started".to_string()),
                    };
                let config_lock = app_state
                    .current_config
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let config_loaded = config_lock.is_some();
                drop(config_lock);
                let config_path = crate::settings::get_last_config_path();
                (executor_running, executor_state, config_loaded, config_path)
            })
            .await
            .map_err(|e| Error::new(format!("spawn_blocking: {}", e)))?;

        let ai_running = {
            let pids = crate::safe_lock::safe_lock_or_recover(
                &state.current_ai_pids,
                "current_ai_pids",
            );
            !pids.is_empty()
        };
        let port = state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed);

        Ok(super::types::GqlRunnerStatus {
            instance_name: None,
            api_port: port as i32,
            executor_running,
            executor_state,
            config_loaded,
            config_path,
            ai_analysis_running: ai_running,
        })
    }

    /// Get orchestration loop status.
    async fn orchestration_loop_status(
        &self,
        ctx: &Context<'_>,
    ) -> Result<super::types::GqlOrchestrationLoopStatus> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let loop_state = state.app_state.orchestration_loop.lock().await;

        Ok(super::types::GqlOrchestrationLoopStatus {
            running: loop_state.running,
            phase: format!("{:?}", loop_state.phase),
            current_iteration: loop_state.current_iteration as i32,
            started_at: loop_state
                .started_at
                .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
            error: loop_state.error.clone(),
        })
    }
}

// ==========================================================================
// Shared Helpers
// ==========================================================================

/// Delegate a command to the UI Bridge via `ui_bridge_request_sync`.
async fn bridge_query(
    state: &Arc<ApiState>,
    command: &str,
    params: serde_json::Value,
) -> Result<Json<serde_json::Value>> {
    ui_bridge::ui_bridge_request_sync(state, command, params)
        .await
        .map(Json)
        .map_err(|e| Error::new(e))
}

/// Build a UiBridgeHealth snapshot from current ApiState.
/// Shared between query resolver and subscription stream.
pub(crate) async fn build_health_snapshot(state: &Arc<ApiState>) -> UiBridgeHealth {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    let now_ms = now_epoch_ms();
    let pong_age_ms = if last_pong > 0 {
        now_ms - last_pong
    } else {
        0
    };
    let responsive = last_pong > 0 && pong_age_ms < 15000;
    let pending_count = pending_count_with_timeout(state).await;
    let cb_state = state.ui_bridge_circuit_breaker.get_state().await;
    let circuit_breaker = convert_cb_state(&cb_state);
    let semaphore_available = state.ui_bridge_semaphore.available_permits() as i32;

    UiBridgeHealth {
        responsive,
        last_heartbeat: last_pong.to_string(),
        heartbeat_age_ms: pong_age_ms.to_string(),
        uptime_seconds: uptime_secs.to_string(),
        pending_requests: pending_count,
        circuit_breaker,
        semaphore_available,
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn pending_count_with_timeout(state: &Arc<ApiState>) -> i32 {
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.ui_bridge_pending.lock(),
    )
    .await
    {
        Ok(pending) => pending.len() as i32,
        Err(_) => -1,
    }
}
