//! OpenTelemetry status endpoint for the MCP API.
//!
//! Exposes the current OTel configuration so that external tooling (e.g. the
//! supervisor dashboard) can display whether trace export is active.

use axum::{response::Json, routing::get};
use serde::Serialize;
use std::sync::Arc;

use crate::mcp::types::ApiState;
use crate::otel::OtelConfig;

#[derive(Serialize)]
pub struct OtelStatus {
    pub enabled: bool,
    pub endpoint: String,
    pub protocol: String,
    pub sampling_rate: f64,
    pub service_name: String,
}

impl From<&OtelConfig> for OtelStatus {
    fn from(cfg: &OtelConfig) -> Self {
        Self {
            enabled: cfg.enabled,
            endpoint: cfg.endpoint.clone(),
            protocol: cfg.protocol.clone(),
            sampling_rate: cfg.sampling_rate,
            service_name: cfg.service_name.clone(),
        }
    }
}

/// GET /otel/status — return the current OpenTelemetry configuration.
///
/// TODO: OtelConfig is not yet persisted in the runner settings file.
/// Once it is added to `RunnerSettings`, this should read from there
/// instead of returning defaults.
pub async fn get_otel_status(
    axum::extract::State(_state): axum::extract::State<Arc<ApiState>>,
) -> Json<OtelStatus> {
    let config = OtelConfig::default();
    Json(OtelStatus::from(&config))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route("/otel/status", get(get_otel_status))
}
