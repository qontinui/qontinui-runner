//! GraphQL mutation resolvers for qontinui-runner.
//!
//! Mutations for UI Bridge actions, circuit breaker control, and element observation.
//! All UI Bridge mutations delegate to existing handlers, preserving reliability features.

use async_graphql::*;
use std::sync::Arc;

use crate::mcp::types::ApiState;

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Reset the UI Bridge circuit breaker to Closed state.
    /// Use this to recover from a tripped circuit breaker without restarting.
    async fn ui_bridge_reset_circuit_breaker(&self, ctx: &Context<'_>) -> Result<bool> {
        let state = ctx.data::<Arc<ApiState>>()?;
        state.ui_bridge_circuit_breaker.reset().await;
        Ok(true)
    }

    /// Simple ping mutation for connectivity testing.
    async fn ping(&self) -> Result<String> {
        Ok("pong".to_string())
    }
}
