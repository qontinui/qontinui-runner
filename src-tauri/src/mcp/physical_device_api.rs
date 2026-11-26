//! HTTP API for physical mobile device management.
//!
//! Provides routes for listing devices, connecting/disconnecting, transport
//! inspection, pairing, and manual LAN device registration.
//!
//! Pairing is the phone's: `POST /ui-bridge/devices/{id}/pair` asks the phone
//! for a token and keeps it, and every later request to that phone off a cable is
//! signed with it ([`crate::mcp::transport::pairing`]).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

use crate::mcp::adb_helper::{self, AdbDeviceInfo};
use crate::mcp::physical_device::{
    ActiveTransport, PhysicalDevice, PhysicalDeviceInfo, PhysicalDeviceRegistry,
};
use crate::mcp::transport::pairing::{self, PairError};
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
    /// Operator remediation hint. Surfaces the most specific actionable cause:
    /// an adb device stuck in the `unauthorized` state (un-accepted "Allow USB
    /// debugging" RSA prompt) takes precedence, since it explains why `adb
    /// forward`/pairing silently fail; otherwise, when no devices are paired,
    /// the transport-setup hint. A release AAB is not auto-reachable until a
    /// transport is set up, so `count: 0` is the common "nothing to test"
    /// state — surface the fix inline instead of leaving callers to guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreferTransportRequest {
    kind: TransportKind,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairDeviceRequest {
    /// The 6-digit code the app's Settings shows after Pair is tapped. Not needed
    /// over a USB cable, where the phone admits this runner as on the phone itself.
    #[serde(default)]
    code: Option<String>,
    /// Replace a token this runner already holds for the device, after the app's
    /// Settings revoked the pairing. Over a USB cable no flag is needed.
    #[serde(default)]
    replace: bool,
}

/// What a pairing did. The token itself is never returned: it stays in this
/// runner's encrypted store and only ever signs requests.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairDeviceResponse {
    /// The registry row that was paired.
    device_id: String,
    /// The phone's own id, which its token is keyed by, as its mDNS record and
    /// its `/ui-bridge/health` publish it.
    ui_bridge_device_id: String,
    /// The transport the pairing went over.
    transport: TransportKind,
    paired: bool,
}

fn default_platform() -> String {
    "android".to_string()
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
pub(crate) struct ConnectDeviceResponse {
    device_id: String,
    proxy_url: String,
    sdk_url: String,
    /// Which bearer the handshake attached: see the `AUTH_PATH_*` constants in
    /// `sdk_client`. `withheld_foreign_host` here: a physical device is not sent
    /// the operator's bearer.
    auth_path: String,
}

// ============================================================================
// Helpers
// ============================================================================

/// Build the "Allow USB debugging" RSA-prompt remediation hint if any adb
/// device is stuck in the `unauthorized` state.
///
/// A USB device that hasn't accepted the RSA authorization prompt shows up in
/// `adb devices` as `<serial>\tunauthorized`. In that state `adb forward` and
/// pairing silently fail, so the generic "run adb forward" hint is misleading —
/// this names the real cause and the fix. Returns `None` when no device is
/// unauthorized.
fn unauthorized_devices_hint(devices: &[AdbDeviceInfo]) -> Option<String> {
    let serials: Vec<&str> = devices
        .iter()
        .filter(|d| d.state == "unauthorized")
        .map(|d| d.serial.as_str())
        .collect();
    if serials.is_empty() {
        return None;
    }
    Some(format!(
        "ADB device(s) {} are UNAUTHORIZED — accept the \"Allow USB debugging\" \
         RSA prompt on the phone (or revoke + re-enable USB debugging in \
         Developer Options, then reconnect), then retry. Until the device is \
         authorized, `adb forward` and pairing cannot succeed.",
        serials.join(", ")
    ))
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
    // An adb device stuck in `unauthorized` (un-accepted "Allow USB debugging"
    // RSA prompt) is the real cause behind an otherwise-opaque USB failure:
    // `adb forward` and pairing silently fail, and the generic "run adb
    // forward" hint below would mislead. Detect it first and surface the
    // actual remedy. Empty on any adb error, so this never masks the paired
    // devices already found.
    let unauthorized_hint = unauthorized_devices_hint(&adb_helper::list_devices().await);
    let hint = if unauthorized_hint.is_some() {
        unauthorized_hint
    } else if count == 0 {
        Some(
            "No devices registered. Set up a transport, then re-query: \
             (USB) plug the phone in with USB debugging allowed, and the runner \
             forwards and registers it, or run `adb forward tcp:8087 tcp:8087` \
             and hit http://localhost:8087/ui-bridge/health; or (LAN) in \
             qontinui-mobile enable Settings -> Developer -> \"Local UI Bridge \
             server\" and wait for the phone to appear here. An app built with \
             @qontinui/ui-bridge-native 0.11.0 or later then needs pairing first: \
             POST /ui-bridge/devices/{id}/pair with {} on the phone's USB row, or \
             on Wi-Fi with the 6-digit code its Settings shows after Pair is \
             tapped. An older app needs no pairing."
                .to_string(),
        )
    } else {
        None
    };
    Ok(Json(ApiResponse::success(DeviceListResponse {
        devices,
        count,
        hint,
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
///
/// Makes the device the active SDK target over its best transport; see
/// [`connect_device_over_transports`].
async fn connect_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ConnectDeviceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match connect_device_over_transports(
        &state.sdk_connection,
        &state.physical_device_registry,
        &id,
    )
    .await
    {
        Ok(resp) => Ok(Json(ApiResponse::success(resp))),
        Err(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Device '{}' not found or has no active transport",
                id
            ))),
        )),
        // A phone that requires pairing answered: the device is there, and this
        // runner has to pair before it can drive it.
        Err(Some(resp)) if refused_for_pairing(&resp) => Err((StatusCode::CONFLICT, Json(resp))),
        Err(Some(resp)) => Err((StatusCode::BAD_GATEWAY, Json(resp))),
    }
}

/// Connect to device `id` over its transports, the active one first. A transport
/// whose phone requires pairing this runner has not done is passed over for the
/// next one, so an unpaired phone on Wi-Fi is still reached over the cloud relay,
/// which its pairing gate does not cover; the transport that answered becomes the
/// device's active one. `Err(None)`: no such device, or one with no transport.
/// `Err(Some(_))`: every transport failed, with the first pairing refusal when
/// there was one, since it says what to do.
pub(crate) async fn connect_device_over_transports(
    sdk_connection: &tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>,
    registry: &PhysicalDeviceRegistry,
    id: &str,
) -> Result<ConnectDeviceResponse, Option<ApiResponse<()>>> {
    let transports: Vec<(TransportKind, String)> = match registry.get_device(id).await {
        Some(device) => device
            .transports_by_preference()
            .into_iter()
            .map(|t| (t.kind, t.proxy_url.clone()))
            .collect(),
        None => Vec::new(),
    };
    let mut first_refusal: Option<ApiResponse<()>> = None;
    let mut last_error: Option<ApiResponse<()>> = None;
    for (index, (kind, proxy_url)) in transports.iter().enumerate() {
        match connect_device_at(sdk_connection, id.to_string(), proxy_url.clone()).await {
            Ok(resp) => {
                if index > 0 {
                    info!(device_id = %id, transport = %kind, "connected over a fallback transport");
                    registry.prefer_transport(id, *kind).await;
                }
                return Ok(resp);
            }
            Err(resp) if refused_for_pairing(&resp) => {
                first_refusal.get_or_insert(resp);
            }
            Err(resp) => last_error = Some(resp),
        }
    }
    Err(first_refusal.or(last_error))
}

/// Whether a connect failed because the phone requires pairing.
fn refused_for_pairing(resp: &ApiResponse<()>) -> bool {
    resp.hint
        .as_ref()
        .and_then(|hint| hint.get("authPath"))
        .and_then(serde_json::Value::as_str)
        == Some(crate::mcp::sdk_client::AUTH_PATH_PAIRING_REQUIRED)
}

/// Make the device at `proxy_url` the active SDK target. The handshake's
/// `auth_path` is surfaced the way `handle_connect` surfaces it: on the response,
/// or in the error envelope's `hint`.
pub(crate) async fn connect_device_at(
    sdk_connection: &tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>,
    id: String,
    proxy_url: String,
) -> Result<ConnectDeviceResponse, ApiResponse<()>> {
    // Connect via the existing SDK connection logic
    match crate::mcp::sdk_client::connect_sdk_app(
        sdk_connection,
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
            info!(device_id = %id, proxy_url = %proxy_url, auth_path = %resp.auth_path, "Physical device connected as SDK target");
            Ok(ConnectDeviceResponse {
                device_id: id,
                proxy_url,
                sdk_url: resp.url,
                auth_path: resp.auth_path,
            })
        }
        Err(e) => {
            warn!(device_id = %id, error = %e, auth_path = %e.auth_path, "Failed to connect physical device");
            let mut resp = ApiResponse::error(format!("Connection failed: {}", e));
            if e.auth_path == crate::mcp::sdk_client::AUTH_PATH_PAIRING_REQUIRED {
                resp.code = Some("PAIRING_REQUIRED".to_string());
            }
            resp.hint = Some(serde_json::json!({ "authPath": e.auth_path }));
            Err(resp)
        }
    }
}

/// POST /ui-bridge/devices/:id/disconnect
///
/// Drops the SDK connection to every one of the device's transports, since a
/// connect may have fallen back from the active one to another.
async fn disconnect_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let Some(device) = state.physical_device_registry.get_device(&id).await else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!("Device '{}' not found", id))),
        ));
    };

    drop_device_connections(&state.sdk_connection, &device).await;

    info!(device_id = %id, "Physical device disconnected");
    Ok(Json(ApiResponse::success(serde_json::json!({
        "deviceId": id,
        "disconnected": true
    }))))
}

/// Drop the SDK connection to every one of `device`'s transports.
async fn drop_device_connections(
    sdk_connection: &tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>,
    device: &PhysicalDevice,
) {
    let mut manager = sdk_connection.lock().await;
    for transport in &device.transports {
        manager.connections.remove(&transport.proxy_url);
        if manager.active_url.as_deref() == Some(transport.proxy_url.as_str()) {
            manager.active_url = None;
        }
    }
}

/// DELETE /ui-bridge/devices/:id
///
/// Unregister a physical device: drop any active SDK connection pointed at its
/// transports, then remove the registry entry entirely. Returns 200 with
/// `{deviceId, removed:true}`; 404 if the device is absent.
///
/// Note: the LAN TCP proxy task spawned at `register-lan` is not currently
/// tracked by the registry (its `JoinHandle` is dropped at creation), so there
/// is no handle to abort here. Removing the registry entry severs all routing
/// to the device; the orphaned proxy listener (if any) is harmless and is reaped
/// on runner shutdown. Tracking proxy handles for explicit teardown is a
/// separate change and intentionally out of scope.
async fn unregister_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // 404 if the device isn't known — distinguish "removed" from "never existed".
    let device = match state.physical_device_registry.get_device(&id).await {
        Some(d) => d,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ApiResponse::error(format!("Device '{}' not found", id))),
            ));
        }
    };

    // Drop any active SDK connection that targets one of this device's
    // transports, so a follow-up SDK request doesn't dangle on a removed device.
    {
        let mut manager = state.sdk_connection.lock().await;
        for transport in &device.transports {
            manager.connections.remove(&transport.proxy_url);
            if manager.active_url.as_deref() == Some(transport.proxy_url.as_str()) {
                manager.active_url = None;
            }
        }
    }

    // Remove the registry entry (this drops all transport metadata for the device).
    state.physical_device_registry.remove_device(&id).await;

    info!(device_id = %id, "Physical device unregistered");
    Ok(Json(ApiResponse::success(serde_json::json!({
        "deviceId": id,
        "removed": true
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
    if state
        .physical_device_registry
        .prefer_transport(&id, req.kind)
        .await
    {
        Ok(Json(ApiResponse::success(serde_json::json!({
            "deviceId": id,
            "activeTransport": req.kind
        }))))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Device '{}' has no {} transport",
                id, req.kind
            ))),
        ))
    }
}

/// POST /ui-bridge/devices/:id/pair
///
/// Ask the phone for a token and keep it, so every later request to it off a
/// cable is signed. Body: `{"code": "123456"}`, the code the app's Settings shows
/// after Pair is tapped; over a USB cable send `{}` or no body, since the phone
/// asks for no code there. A code-less pair over anything but a cable is refused
/// before anything is sent, since only an impostor answers one off a cable. A
/// local emulator needs no pairing: it admits this runner as on the phone itself.
/// Pairing again is safe: the phone holds one token and hands that same one to
/// every pairing, so a re-pair never invalidates another runner. It changes only
/// when the app revokes pairing, after which this runner's requests are refused
/// until it pairs again, over a cable or with `"replace": true`.
///
/// Goes over the device's best transport other than the cloud relay, which the
/// phone's gate does not cover. The token is never returned. On success that
/// transport becomes the device's active one again (a connect made while it was
/// unpaired may have fallen back to the relay) and every live connection to the
/// device is dropped, so the next connect goes to it, signed.
async fn pair_device(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<ApiResponse<PairDeviceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Parsed here rather than by axum's `Json`, whose rejection is plain text
    // rather than this route's envelope.
    let req = if body.iter().all(u8::is_ascii_whitespace) {
        PairDeviceRequest::default()
    } else {
        match serde_json::from_slice::<PairDeviceRequest>(&body) {
            Ok(req) => req,
            Err(e) => {
                let mut resp = ApiResponse::error(format!(
                    "The body is {{}} over a cable, or {{\"code\": \"123456\"}}: {e}"
                ));
                resp.code = Some("PAIRING_BODY_MALFORMED".to_string());
                return Err((StatusCode::BAD_REQUEST, Json(resp)));
            }
        }
    };
    pair_device_with(
        &state.sdk_connection,
        &state.physical_device_registry,
        &pairing::device_tokens(),
        id,
        req.code,
        req.replace,
    )
    .await
    .map(|paired| Json(ApiResponse::success(paired)))
    .map_err(|(status, resp)| (status, Json(resp)))
}

/// What `POST /ui-bridge/devices/{id}/pair` does, with the token store passed in.
pub(crate) async fn pair_device_with(
    sdk_connection: &tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>,
    registry: &PhysicalDeviceRegistry,
    store: &pairing::DeviceTokenStore,
    id: String,
    code: Option<String>,
    replace: bool,
) -> Result<PairDeviceResponse, (StatusCode, ApiResponse<()>)> {
    let refuse = |status: StatusCode, code: &str, message: String| {
        let mut resp = ApiResponse::error(message);
        resp.code = Some(code.to_string());
        (status, resp)
    };
    if let Some(code) = &code {
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refuse(
                StatusCode::BAD_REQUEST,
                "PAIRING_CODE_MALFORMED",
                "The pairing code is the 6 digits the app's Settings shows".to_string(),
            ));
        }
    }
    let Some(device) = registry.get_device(&id).await else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "DEVICE_NOT_FOUND",
            format!("Device '{id}' not found"),
        ));
    };
    let Some(transport) = device
        .transports_by_preference()
        .into_iter()
        .find(|t| t.kind != TransportKind::Cloud)
        .cloned()
    else {
        return Err(refuse(
            StatusCode::CONFLICT,
            "PAIRING_NOT_APPLICABLE",
            format!(
                "Device '{id}' is reached only over the cloud relay, which needs no pairing. \
                 Pairing is for a phone reached on the local network."
            ),
        ));
    };

    let over_cable = crate::mcp::transport::is_cable_url_now(&transport.proxy_url).await;
    if code.is_none() && !over_cable {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PAIRING_CODE_REQUIRED",
            format!(
                "Device '{id}' is reached over {}, not a USB cable, so pairing needs the 6-digit \
                 code the app's Settings shows after Pair is tapped: send it as \
                 {{\"code\": \"123456\"}}.",
                transport.kind
            ),
        ));
    }
    let options = pairing::PairOptions {
        code: code.as_deref(),
        explicit: true,
        expected_device_id: None,
        may_replace: replace || over_cable,
    };
    match pairing::pair_with_device(&transport.proxy_url, store, options).await {
        Ok(paired) => {
            registry.prefer_transport(&id, transport.kind).await;
            drop_device_connections(sdk_connection, &device).await;
            info!(device_id = %id, ui_bridge_device_id = %paired.device_id, transport = %transport.kind, "device paired");
            Ok(PairDeviceResponse {
                device_id: id,
                ui_bridge_device_id: paired.device_id,
                transport: transport.kind,
                paired: true,
            })
        }
        Err(e) => {
            warn!(device_id = %id, code = e.code(), "pairing failed");
            let status = match &e {
                PairError::AppTooOld | PairError::TokenHeld { .. } => StatusCode::CONFLICT,
                PairError::Refused { status: 429, .. } => StatusCode::TOO_MANY_REQUESTS,
                // The phone's secure store is not ready: worth a retry, not a new code.
                PairError::Refused { status: 503, .. } => StatusCode::SERVICE_UNAVAILABLE,
                PairError::Refused { .. } => StatusCode::UNPROCESSABLE_ENTITY,
                PairError::Unreachable(_) | PairError::BadAnswer(_) => StatusCode::BAD_GATEWAY,
                PairError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err(refuse(status, e.code(), e.message()))
        }
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
        .route(
            "/ui-bridge/devices/{id}",
            get(get_device).delete(unregister_device),
        )
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
        .route("/ui-bridge/devices/{id}/pair", post(pair_device))
        .route("/ui-bridge/devices/register-lan", post(register_lan_device))
}

#[cfg(test)]
mod tests {
    use super::{unauthorized_devices_hint, AdbDeviceInfo};

    fn dev(serial: &str, state: &str) -> AdbDeviceInfo {
        AdbDeviceInfo {
            serial: serial.to_string(),
            state: state.to_string(),
            model: None,
        }
    }

    #[test]
    fn unauthorized_line_produces_rsa_hint() {
        // Mirrors an `adb devices` line: `R3CN30ABCDEF\tunauthorized`.
        let devices = vec![dev("R3CN30ABCDEF", "unauthorized")];
        let hint = unauthorized_devices_hint(&devices).expect("hint present");
        assert!(hint.contains("UNAUTHORIZED"));
        assert!(hint.contains("RSA prompt"));
        assert!(hint.contains("R3CN30ABCDEF"));
    }

    #[test]
    fn multiple_unauthorized_serials_listed() {
        let devices = vec![
            dev("AAA111", "unauthorized"),
            dev("BBB222", "device"),
            dev("CCC333", "unauthorized"),
        ];
        let hint = unauthorized_devices_hint(&devices).expect("hint present");
        assert!(hint.contains("AAA111"));
        assert!(hint.contains("CCC333"));
        // Authorized device is not named in the unauthorized hint.
        assert!(!hint.contains("BBB222"));
    }

    #[test]
    fn no_unauthorized_yields_none() {
        // Authorized + offline devices must not trigger the RSA hint.
        let devices = vec![dev("AAA111", "device"), dev("BBB222", "offline")];
        assert!(unauthorized_devices_hint(&devices).is_none());
        assert!(unauthorized_devices_hint(&[]).is_none());
    }
}
