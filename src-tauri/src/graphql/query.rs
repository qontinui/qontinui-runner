//! GraphQL query resolvers for qontinui-runner.
//!
//! All UI Bridge query resolvers delegate to the existing `ui_bridge_request_sync()`
//! function, preserving circuit breaker, semaphore, dedup, and error classification.

use async_graphql::*;
use std::sync::Arc;

use crate::mcp::types::ApiState;
use crate::mcp::ui_bridge::CircuitBreakerState as CbState;

use super::types::{CircuitBreakerState, UiBridgeHealth};

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
    /// Get the current health status of the UI Bridge relay.
    /// Includes circuit breaker state, semaphore availability, and frontend responsiveness.
    async fn ui_bridge_health(&self, ctx: &Context<'_>) -> Result<UiBridgeHealth> {
        let state = ctx.data::<Arc<ApiState>>()?;
        Ok(build_health_snapshot(state).await)
    }

    /// Get UI Bridge diagnostics as raw JSON (full diagnostics endpoint).
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
