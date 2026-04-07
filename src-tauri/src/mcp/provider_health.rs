//! Provider health endpoint exposing AI provider circuit breaker states.

use axum::{response::IntoResponse, Json};

pub async fn get_provider_health() -> impl IntoResponse {
    Json(crate::ai_provider::circuit_breaker::all_provider_circuit_states())
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new().route("/provider-health", get(get_provider_health))
}
