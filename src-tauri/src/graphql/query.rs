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
                responsive: manager.active_responsive.unwrap_or(false),
                is_active: active_url.as_deref() == Some(&conn.app_url),
            });
        }
        Ok(connections)
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
