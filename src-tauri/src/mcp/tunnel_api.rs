//! HTTP routes for managing the rathole reverse tunnel (Plan 1B).

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

use crate::mcp::types::{ApiResponse, ApiState};
use crate::tunnel::{RatholeConfig, TunnelService};

// ----------------------------------------------------------------------------
// Request / response shapes
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTunnelRequest {
    pub server_addr: String,
    #[serde(default)]
    pub default_token: Option<String>,
    pub services: Vec<TunnelService>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateConfigRequest {
    pub server_addr: String,
    #[serde(default)]
    pub default_token: Option<String>,
    pub services: Vec<TunnelService>,
    /// Optional: bind host for the server TOML (default `0.0.0.0`).
    #[serde(default)]
    pub server_bind_host: Option<String>,
    /// Optional: starting public port for server-side services
    /// (default `5200`, incremented per service).
    #[serde(default)]
    pub public_bind_start: Option<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateConfigResponse {
    pub client_toml: String,
    /// Mirror server-side TOML for the user's rathole VPS (Plan 1B polish).
    pub server_toml: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatusResponse {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_addr: Option<String>,
    pub services: Vec<TunnelService>,
}

// ----------------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------------

async fn start_tunnel(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<StartTunnelRequest>,
) -> Result<Json<ApiResponse<TunnelStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let config = RatholeConfig {
        server_addr: req.server_addr.clone(),
        default_token: req.default_token,
        services: req.services.clone(),
    };

    state.tunnel_client.start(config).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!("tunnel start failed: {e}"))),
        )
    })?;

    Ok(Json(ApiResponse::success(TunnelStatusResponse {
        connected: state.tunnel_client.is_connected().await,
        server_addr: Some(req.server_addr),
        services: req.services,
    })))
}

async fn stop_tunnel(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    state.tunnel_client.stop().await;
    Ok(Json(ApiResponse::success(())))
}

async fn tunnel_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<TunnelStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let connected = state.tunnel_client.is_connected().await;
    let config = state.tunnel_client.current_config().await;
    Ok(Json(ApiResponse::success(TunnelStatusResponse {
        connected,
        server_addr: config.as_ref().map(|c| c.server_addr.clone()),
        services: config.map(|c| c.services).unwrap_or_default(),
    })))
}

async fn generate_config(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<GenerateConfigRequest>,
) -> Result<Json<ApiResponse<GenerateConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let bind_host = req.server_bind_host.as_deref().unwrap_or("0.0.0.0");
    let public_start = req.public_bind_start.unwrap_or(5200);
    let config = RatholeConfig {
        server_addr: req.server_addr,
        default_token: req.default_token,
        services: req.services,
    };
    let client_toml = config.to_toml();
    let server_toml = config.to_server_toml(bind_host, public_start);
    Ok(Json(ApiResponse::success(GenerateConfigResponse {
        client_toml,
        server_toml,
    })))
}

// ----------------------------------------------------------------------------
// Routes
// ----------------------------------------------------------------------------

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/tunnel/start", post(start_tunnel))
        .route("/tunnel/stop", post(stop_tunnel))
        .route("/tunnel/status", get(tunnel_status))
        .route("/tunnel/generate-config", post(generate_config))
}
