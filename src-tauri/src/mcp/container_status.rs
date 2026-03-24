use axum::{extract::State as AxumState, response::IntoResponse, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::mcp::types::ApiState;

#[derive(Serialize)]
pub struct ContainerStatus {
    pub docker_available: bool,
    pub isolation_enabled: bool,
    pub base_image: String,
    pub executor_ready: bool,
}

pub async fn get_container_status(
    AxumState(state): AxumState<Arc<ApiState>>,
) -> impl IntoResponse {
    let config = crate::settings::get_container_settings();

    // Check if the executor is initialized and available
    let executor_guard = state.app_state.container_executor.lock().await;
    let (docker_available, executor_ready) = match executor_guard.as_ref() {
        Some(executor) => {
            let available = executor.is_available();
            (available, available)
        }
        None => (false, false),
    };

    Json(ContainerStatus {
        docker_available,
        isolation_enabled: config.enabled,
        base_image: config.base_image,
        executor_ready,
    })
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new().route("/container/status", get(get_container_status))
}
