//! Provider health endpoint exposing AI provider circuit breaker states.

use axum::{extract::Path, response::IntoResponse, Json};

pub async fn get_provider_health() -> impl IntoResponse {
    Json(crate::ai_provider::circuit_breaker::all_provider_circuit_states())
}

/// Reset a single provider's circuit breaker to Closed.
pub async fn reset_provider_health(Path(provider_key): Path<String>) -> impl IntoResponse {
    crate::ai_provider::circuit_breaker::reset_provider(&provider_key);
    Json(serde_json::json!({ "success": true, "provider_key": provider_key }))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/provider-health", get(get_provider_health))
        .route(
            "/provider-health/{provider_key}/reset",
            post(reset_provider_health),
        )
}
