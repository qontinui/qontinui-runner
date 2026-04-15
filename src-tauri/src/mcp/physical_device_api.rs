//! HTTP API for physical mobile device management.
//!
//! Provides routes for listing devices, connecting/disconnecting, transport
//! inspection, pairing, and manual LAN device registration.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

use crate::mcp::physical_device::{
    ActiveTransport, PhysicalDevice, PhysicalDeviceInfo, PhysicalDeviceRegistry,
};
use crate::mcp::transport::pairing::PairingManager;
use crate::mcp::transport::{DeviceOs, TransportKind};
use crate::mcp::types::{ApiResponse, ApiState};

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceListResponse {
    devices: Vec<PhysicalDevice>,
    count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreferTransportRequest {
    kind: TransportKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitiatePairingRequest {
    device_id: String,
    transport: TransportKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmPairingRequest {
    device_id: String,
    code: String,
    #[serde(default = "default_display_name")]
    display_name: String,
    #[serde(default = "default_platform")]
    platform: String,
}

fn default_display_name() -> String {
    "Unknown Device".to_string()
}

fn default_platform() -> String {
    "android".to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingInitiatedResponse {
    device_id: String,
    code: String,
    expires_at: i64,
    transport: TransportKind,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingConfirmedResponse {
    device_id: String,
    device_token: String,
    paired_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterLanRequest {
    /// IP:port of the device (e.g. "192.168.1.42:8087")
    address: String,
    /// Optional human-readable device ID — defaults to the address
    #[serde(default)]
    device_id: Option<String>,
    /// "android" or "ios"
    #[serde(default = "default_platform")]
    platform: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportInfoResponse {
    transports: Vec<ActiveTransport>,
    active_transport_idx: usize,
    active_proxy_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectDeviceResponse {
    device_id: String,
    proxy_url: String,
    sdk_url: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /ui-bridge/devices
async fn list_devices(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DeviceListResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let devices = state.physical_device_registry.list_all().await;
    let count = devices.len();
    Ok(Json(ApiResponse::success(DeviceListResponse {
        devices,
        count,
    })))
}

/// GET /ui-bridge/devices/:id
async fn get_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<PhysicalDevice>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.physical_device_registry.get_device(&id).await {
        Some(device) => Ok(Json(ApiResponse::success(device))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!("Device '{}' not found", id))),
        )),
    }
}

/// POST /ui-bridge/devices/:id/connect
/// Looks up the device's active proxy URL and makes it the active SDK target.
async fn connect_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ConnectDeviceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let proxy_url = match state
        .physical_device_registry
        .get_active_proxy_url(&id)
        .await
    {
        Some(url) => url,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ApiResponse::error(format!(
                    "Device '{}' not found or has no active transport",
                    id
                ))),
            ));
        }
    };

    // Connect via the existing SDK connection logic
    match crate::mcp::sdk_client::connect_sdk_app(
        &state.sdk_connection,
        &proxy_url,
        None,
        None,
        None,
        Some("mobile".to_string()),
        None,
    )
    .await
    {
        Ok(resp) => {
            info!(device_id = %id, proxy_url = %proxy_url, "Physical device connected as SDK target");
            Ok(Json(ApiResponse::success(ConnectDeviceResponse {
                device_id: id,
                proxy_url,
                sdk_url: resp.url,
            })))
        }
        Err(e) => {
            warn!(device_id = %id, error = %e, "Failed to connect physical device");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(ApiResponse::error(format!("Connection failed: {}", e))),
            ))
        }
    }
}

/// POST /ui-bridge/devices/:id/disconnect
async fn disconnect_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Find the proxy URL for this device
    let proxy_url = match state
        .physical_device_registry
        .get_active_proxy_url(&id)
        .await
    {
        Some(url) => url,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ApiResponse::error(format!("Device '{}' not found", id))),
            ));
        }
    };

    // Remove from SDK connection manager if it's the active connection
    {
        let mut manager = state.sdk_connection.lock().await;
        manager.connections.remove(&proxy_url);
        if manager.active_url.as_deref() == Some(&proxy_url) {
            manager.active_url = None;
        }
    }

    info!(device_id = %id, "Physical device disconnected");
    Ok(Json(ApiResponse::success(serde_json::json!({
        "deviceId": id,
        "disconnected": true
    }))))
}

/// GET /ui-bridge/devices/:id/transport
async fn get_transport_info(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<TransportInfoResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.physical_device_registry.get_device(&id).await {
        Some(device) => {
            let active_proxy_url = device.active_proxy_url().map(String::from);
            Ok(Json(ApiResponse::success(TransportInfoResponse {
                transports: device.transports,
                active_transport_idx: device.active_transport_idx,
                active_proxy_url,
            })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!("Device '{}' not found", id))),
        )),
    }
}

/// POST /ui-bridge/devices/:id/transport/prefer
async fn prefer_transport(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<PreferTransportRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Find the URL for the requested transport kind
    let proxy_url = {
        match state.physical_device_registry.get_device(&id).await {
            Some(device) => device
                .transports
                .iter()
                .find(|t| t.kind == req.kind)
                .map(|t| t.proxy_url.clone()),
            None => None,
        }
    };

    match proxy_url {
        Some(url) => {
            state
                .physical_device_registry
                .promote_transport(&id, req.kind, url)
                .await;
            Ok(Json(ApiResponse::success(serde_json::json!({
                "deviceId": id,
                "activeTransport": req.kind
            }))))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Device '{}' has no {} transport",
                id, req.kind
            ))),
        )),
    }
}

/// POST /ui-bridge/devices/pair/initiate
async fn initiate_pairing(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<InitiatePairingRequest>,
) -> Result<Json<ApiResponse<PairingInitiatedResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pending = state
        .pairing_manager
        .initiate(&req.device_id, req.transport)
        .await;

    info!(
        device_id = %req.device_id,
        code = %pending.code,
        "Pairing initiated"
    );

    Ok(Json(ApiResponse::success(PairingInitiatedResponse {
        device_id: pending.device_id,
        code: pending.code,
        expires_at: pending.expires_at,
        transport: pending.transport_hint,
    })))
}

/// POST /ui-bridge/devices/pair/confirm
async fn confirm_pairing(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ConfirmPairingRequest>,
) -> Result<Json<ApiResponse<PairingConfirmedResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .pairing_manager
        .confirm(&req.device_id, &req.code, &req.display_name, &req.platform)
        .await
    {
        Ok(device) => {
            info!(device_id = %device.device_id, "Pairing confirmed");
            Ok(Json(ApiResponse::success(PairingConfirmedResponse {
                device_id: device.device_id,
                device_token: device.device_token,
                paired_at: device.paired_at,
            })))
        }
        Err(e) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiResponse::error(format!("Pairing failed: {}", e))),
        )),
    }
}

/// DELETE /ui-bridge/devices/:id/pairing
async fn revoke_pairing(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let removed = state.pairing_manager.revoke(&id).await;
    if removed {
        info!(device_id = %id, "Pairing revoked");
        Ok(Json(ApiResponse::success(
            serde_json::json!({ "deviceId": id, "revoked": true }),
        )))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "No pairing found for device '{}'",
                id
            ))),
        ))
    }
}

/// POST /ui-bridge/devices/register-lan
///
/// Manually add a LAN device by IP:port.  Validates reachability, starts a
/// TCP proxy, and registers the device in the registry.
async fn register_lan_device(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RegisterLanRequest>,
) -> Result<Json<ApiResponse<PhysicalDevice>>, (StatusCode, Json<ApiResponse<()>>)> {
    let address: SocketAddr = req.address.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error(format!(
                "Invalid address '{}' — expected IP:port",
                req.address
            ))),
        )
    })?;

    let lan = crate::mcp::transport::lan::LanTransport::new();

    // Validate the device is reachable
    let discovered = lan.check_device(address).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiResponse::error(format!("Device not reachable: {}", e))),
        )
    })?;

    // Start local TCP proxy
    let (local_port, _proxy_handle) = lan.create_proxy(address).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!("Proxy creation failed: {}", e))),
        )
    })?;

    let device_id = req
        .device_id
        .unwrap_or_else(|| format!("lan-{}", address.ip().to_string().replace('.', "-")));

    let os = if req.platform.to_lowercase() == "ios" {
        DeviceOs::Ios
    } else {
        DeviceOs::Android
    };

    let now = chrono::Utc::now().timestamp_millis();

    let info = PhysicalDeviceInfo {
        id: device_id.clone(),
        os,
        device_kind: "physical".to_string(),
        model: None,
        app_id: Some(discovered.app_id),
        ui_bridge_version: discovered.version,
        first_seen_at: now,
        pairing_token: None,
    };

    let transport = ActiveTransport {
        kind: TransportKind::Lan,
        proxy_url: format!("http://127.0.0.1:{}", local_port),
        established_at: now,
        last_healthy_at: now,
        fail_count: 0,
    };

    state
        .physical_device_registry
        .register(info, transport)
        .await;

    let device = state
        .physical_device_registry
        .get_device(&device_id)
        .await
        .expect("just registered");

    info!(
        device_id = %device_id,
        local_port,
        remote = %address,
        "LAN device registered"
    );

    Ok(Json(ApiResponse::success(device)))
}

// ============================================================================
// Route registration
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/devices", get(list_devices))
        .route("/ui-bridge/devices/{id}", get(get_device))
        .route("/ui-bridge/devices/{id}/connect", post(connect_device))
        .route(
            "/ui-bridge/devices/{id}/disconnect",
            post(disconnect_device),
        )
        .route("/ui-bridge/devices/{id}/transport", get(get_transport_info))
        .route(
            "/ui-bridge/devices/{id}/transport/prefer",
            post(prefer_transport),
        )
        .route("/ui-bridge/devices/pair/initiate", post(initiate_pairing))
        .route("/ui-bridge/devices/pair/confirm", post(confirm_pairing))
        .route("/ui-bridge/devices/{id}/pairing", delete(revoke_pairing))
        .route("/ui-bridge/devices/register-lan", post(register_lan_device))
}
