//! Device-bridge snapshot consumer — capture a cloud-relayed device's UI Bridge
//! snapshot **by deviceId**, on demand.
//!
//! This is the read-path twin of the background cloud device-bridge poller in
//! `mcp_api::api_state_init` (≈2245-2353): instead of registering every cloud
//! device into the physical-device registry for the app's lifetime, it opens a
//! one-shot tunnel for a single requested `deviceId`, GETs that device's UI
//! Bridge snapshot through the tunnel, then tears the tunnel down. It is the
//! shared unlock for verifying the app-generator (mobile) and comprehension
//! (web) against *real* device snapshots — the snapshot returned here is the
//! mobile's element tree, not the runner's own webview.
//!
//! ## Composition (mirrors `mcp_api.rs:2259-2334`)
//!
//! 1. `token = AuthManager::new().get_access_token()?` (`auth.rs:204`).
//! 2. `CloudRegistryPoller::new(backend_url, _).poll(&token)` (
//!    `mcp/discovery/cloud_registry.rs:53`) → `Vec<CloudDeviceInfo>`; locate the
//!    one whose `device_id == deviceId`.
//! 3. `CloudTransport::new(backend_url).open_tunnel(&device_id, &token)` (
//!    `mcp/transport/cloud.rs:95`) → `local_port`; the tunnel forwards HTTP to
//!    the device's UI-Bridge server.
//! 4. HTTP GET `http://127.0.0.1:{local_port}/ui-bridge/control/snapshot` — the
//!    native SDK serves the snapshot at `control/snapshot` (
//!    `@qontinui/ui-bridge-native` route table: `getSnapshot` handler, full path
//!    `/ui-bridge/control/snapshot`). Parse the JSON snapshot body.
//! 5. `transport.close_tunnel(&device_id)` (drives `TunnelHandle::shutdown`).
//!
//! Endpoints:
//! - `GET /ui-bridge/cloud-devices` — list cloud-relayed devices (id, name,
//!   platform, app_id).
//! - `GET /ui-bridge/cloud-snapshot?deviceId=<id>` — capture that device's
//!   UI Bridge snapshot.
//!
//! Both are runner-only (not in the SDK `UI_BRIDGE_ROUTES` contract — there is
//! no SDK-side analog for "reach a cloud-relayed device by id"), so they are
//! allow-listed in `runner_only_baseline` of
//! `sdk_manifest_routes_are_exposed_by_runner`.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::mcp::discovery::cloud_registry::CloudDeviceInfo;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Path served by `@qontinui/ui-bridge-native` for the full UI snapshot
/// (`getSnapshot` handler). The tunnel forwards the raw HTTP path verbatim to
/// the device's UI Bridge server, so this is the device-relative path — the
/// same surface the background poller health-checks at `/ui-bridge/health`.
const DEVICE_SNAPSHOT_PATH: &str = "/ui-bridge/control/snapshot";

/// How long to wait for the snapshot GET through the tunnel. The cloud transport
/// itself times out a tunneled request at 5s (`cloud.rs:412`); give the outer
/// reqwest a slightly longer ceiling so the tunnel's own 504 surfaces first.
const SNAPSHOT_GET_TIMEOUT: Duration = Duration::from_secs(8);

/// Query params for `GET /ui-bridge/cloud-snapshot`.
#[derive(Debug, Deserialize)]
pub struct CloudSnapshotQuery {
    /// The cloud-registered device id to capture (e.g. `RFCW6041HHN`).
    #[serde(alias = "device_id")]
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

/// Resolve the backend URL the poller + transport should use, mirroring
/// `mcp_api.rs`: the poller uses `CloudRelaySettings.backend_url` (the host that
/// serves `/api/v1/device-bridge/available-devices`); the transport uses
/// `get_api_base_url()` for the WS tunnel (runner is co-located with the backend
/// in the deployed topology; the env override keeps a temp runner pointed at the
/// same prod relay).
fn poller_backend_url() -> String {
    let cloud_settings = crate::config_facade::get_setting::<crate::settings::CloudRelaySettings>();
    cloud_settings.backend_url
}

fn transport_backend_url() -> String {
    crate::api_config::get_api_base_url()
}

/// Project a [`CloudDeviceInfo`] into the compact JSON the `/cloud-devices`
/// endpoint returns. Keeps the wire shape stable and decoupled from the
/// poller's internal struct.
fn device_summary(d: &CloudDeviceInfo) -> serde_json::Value {
    serde_json::json!({
        "deviceId": d.device_id,
        "displayName": d.display_name,
        "platform": d.platform,
        "appId": d.app_id,
        "connectedSince": d.connected_since,
    })
}

/// `GET /ui-bridge/cloud-devices`
///
/// Polls the relay backend for cloud-registered devices and returns the list.
/// This is the discovery leg — call it first to confirm the target device
/// appears before requesting its snapshot.
pub async fn ui_bridge_cloud_devices_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: cloud-devices");

    let token = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) => t,
        Err(e) => {
            warn!("cloud-devices: no access token: {}", e);
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(api_error(format!(
                    "runner is not cloud-authed (no access token): {e}"
                ))),
            ));
        }
    };

    let poller =
        crate::mcp::discovery::cloud_registry::CloudRegistryPoller::new(poller_backend_url(), 30);

    match poller.poll(&token).await {
        Ok(devices) => {
            let summaries: Vec<serde_json::Value> = devices.iter().map(device_summary).collect();
            Ok(Json(ApiResponse::success(serde_json::json!({
                "devices": summaries,
                "count": summaries.len(),
            }))))
        }
        Err(e) => {
            error!("cloud-devices: poll failed: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("cloud registry poll failed: {e}"))),
            ))
        }
    }
}

/// `GET /ui-bridge/cloud-snapshot?deviceId=<id>`
///
/// Opens a one-shot cloud tunnel for `deviceId`, GETs the device's UI Bridge
/// snapshot through it, tears the tunnel down, and returns the parsed snapshot.
/// The returned body is the device's own element tree — distinguishable from a
/// runner webview snapshot by its native fields (`registration`, native element
/// `type`/`identifier`, mobile `currentRoute`, optional `appInfo`).
pub async fn ui_bridge_cloud_snapshot_handler(
    State(_state): State<Arc<ApiState>>,
    Query(q): Query<CloudSnapshotQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let device_id = q.device_id.trim().to_string();
    info!("UI Bridge API: cloud-snapshot -> {}", device_id);

    if device_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("missing required query param `deviceId`")),
        ));
    }

    // 1. Auth token.
    let token = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) => t,
        Err(e) => {
            warn!("cloud-snapshot: no access token: {}", e);
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(api_error(format!(
                    "runner is not cloud-authed (no access token): {e}"
                ))),
            ));
        }
    };

    // 2. Poll the registry and locate the requested device.
    let poller =
        crate::mcp::discovery::cloud_registry::CloudRegistryPoller::new(poller_backend_url(), 30);
    let devices = match poller.poll(&token).await {
        Ok(d) => d,
        Err(e) => {
            error!("cloud-snapshot: registry poll failed: {}", e);
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("cloud registry poll failed: {e}"))),
            ));
        }
    };

    if !devices.iter().any(|d| d.device_id == device_id) {
        let known: Vec<&str> = devices.iter().map(|d| d.device_id.as_str()).collect();
        warn!(
            "cloud-snapshot: device {} not in available-devices (known: {:?})",
            device_id, known
        );
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "device `{device_id}` not in cloud available-devices (known: {known:?})"
            ))),
        ));
    }

    // 3. Open a one-shot tunnel for this device.
    let transport = crate::mcp::transport::cloud::CloudTransport::new(transport_backend_url());
    let local_port = match transport.open_tunnel(&device_id, &token).await {
        Ok(p) => p,
        Err(e) => {
            error!("cloud-snapshot: open_tunnel({}) failed: {}", device_id, e);
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("failed to open cloud tunnel: {e}"))),
            ));
        }
    };

    // 4. GET the device's snapshot through the tunnel.
    let result = fetch_snapshot_via_tunnel(local_port).await;

    // 5. Always tear the tunnel down, success or failure.
    transport.close_tunnel(&device_id).await;

    match result {
        Ok(snapshot) => {
            info!(
                "cloud-snapshot: captured {} ({} elements)",
                device_id,
                snapshot
                    .get("elements")
                    .and_then(|e| e.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0)
            );
            Ok(Json(ApiResponse::success(serde_json::json!({
                "deviceId": device_id,
                "source": "cloud-relay",
                "snapshotPath": DEVICE_SNAPSHOT_PATH,
                "snapshot": snapshot,
            }))))
        }
        Err(e) => {
            error!("cloud-snapshot: fetch through tunnel failed: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!(
                    "tunnel opened but snapshot GET failed: {e}"
                ))),
            ))
        }
    }
}

/// GET `http://127.0.0.1:{local_port}{DEVICE_SNAPSHOT_PATH}` through the cloud
/// tunnel and parse the JSON body. Pulled out so the parse/status handling is
/// unit-testable against a local mock server without a live relay.
async fn fetch_snapshot_via_tunnel(local_port: u16) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{local_port}{DEVICE_SNAPSHOT_PATH}");
    let client = reqwest::Client::builder()
        .timeout(SNAPSHOT_GET_TIMEOUT)
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("read body failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("device returned HTTP {status}: {body}"));
    }

    parse_snapshot_body(&body)
}

/// Parse a snapshot response body into JSON, validating it carries the minimal
/// snapshot shape (a top-level `elements` array). Rejects a non-snapshot body
/// (e.g. an HTML error page from a mis-routed tunnel) loudly rather than passing
/// garbage through.
fn parse_snapshot_body(body: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        format!(
            "snapshot body is not JSON ({e}); first 200 bytes: {:.200}",
            body
        )
    })?;

    // The native SDK snapshot always carries `elements` (an array). Some
    // transports wrap the payload in `{ success, data: { ... } }`; unwrap a
    // single `data` level if the top level isn't the snapshot itself.
    let snapshot = if value.get("elements").is_some() {
        value
    } else if let Some(data) = value.get("data") {
        if data.get("elements").is_some() {
            data.clone()
        } else {
            return Err(format!(
                "response JSON has no `elements` array (top-level keys: {:?})",
                top_level_keys(&value)
            ));
        }
    } else {
        return Err(format!(
            "response JSON has no `elements` array (top-level keys: {:?})",
            top_level_keys(&value)
        ));
    };

    Ok(snapshot)
}

fn top_level_keys(v: &serde_json::Value) -> Vec<String> {
    v.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `GET /ui-bridge/cloud-auth-status`
///
/// Diagnostic for the cloud-relay auth path: surfaces the backend URLs the
/// consumer targets, the relevant [`crate::settings::CloudRelaySettings`] flags,
/// and whether an access token is retrievable + its expiry — so an opaque `401`
/// from `/cloud-devices` becomes actionable (wrong backend/env vs an expired or
/// absent token). Returns metadata only; the token value is never exposed.
pub async fn ui_bridge_cloud_auth_status_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: cloud-auth-status");

    let cloud_settings = crate::config_facade::get_setting::<crate::settings::CloudRelaySettings>();

    let auth = crate::auth::AuthManager::new();
    let token_res = auth.get_access_token();
    let has_token = token_res.is_ok();
    let token_error = token_res.as_ref().err().map(|e| e.to_string());
    let token_exp = auth.access_token_exp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let token_expired = token_exp.map(|exp| exp <= now);
    let expires_in_secs = token_exp.map(|exp| exp - now);

    Json(ApiResponse::success(serde_json::json!({
        "pollerBackendUrl": poller_backend_url(),
        "transportBackendUrl": transport_backend_url(),
        "cloudRelayEnabled": cloud_settings.enabled,
        "deviceBridgeEnabled": cloud_settings.device_bridge_enabled,
        "hasAccessToken": has_token,
        "tokenError": token_error,
        "tokenExp": token_exp,
        "tokenExpired": token_expired,
        "expiresInSecs": expires_in_secs,
        "now": now,
    })))
}

/// Cloud-snapshot consumer routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/ui-bridge/cloud-devices",
            get(ui_bridge_cloud_devices_handler),
        )
        .route(
            "/ui-bridge/cloud-snapshot",
            get(ui_bridge_cloud_snapshot_handler),
        )
        .route(
            "/ui-bridge/cloud-auth-status",
            get(ui_bridge_cloud_auth_status_handler),
        )
}

/// Static (method, path) tuples mirroring `routes()`. Concatenated into
/// `route_manifest()` in `mod.rs`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/cloud-devices"),
        ("GET", "/ui-bridge/cloud-snapshot"),
        ("GET", "/ui-bridge/cloud-auth-status"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_bare_native_snapshot() {
        // The shape `@qontinui/ui-bridge-native` `createSnapshot` emits.
        let body = json!({
            "timestamp": 1_700_000_000_000i64,
            "elements": [
                { "id": "home-title", "type": "text", "label": "Welcome",
                  "identifier": "home-title", "visibility": "visible" },
                { "id": "login-btn", "type": "button", "label": "Log in",
                  "identifier": "login-btn", "actions": ["press"],
                  "visibility": "visible" }
            ],
            "components": [],
            "workflows": [],
            "currentRoute": "/home",
            "registration": { "total": 2, "registered": 2 },
            "appInfo": { "appId": "com.qontinui.mobile" }
        })
        .to_string();

        let snap = parse_snapshot_body(&body).expect("parses bare snapshot");
        assert_eq!(snap["elements"].as_array().unwrap().len(), 2);
        assert_eq!(snap["currentRoute"], "/home");
        assert_eq!(snap["appInfo"]["appId"], "com.qontinui.mobile");
    }

    #[test]
    fn unwraps_single_data_envelope() {
        // Some transports wrap the snapshot in { success, data: { ... } }.
        let body = json!({
            "success": true,
            "data": {
                "timestamp": 1i64,
                "elements": [ { "id": "x", "type": "view" } ],
                "currentRoute": "/"
            }
        })
        .to_string();

        let snap = parse_snapshot_body(&body).expect("unwraps data envelope");
        assert_eq!(snap["elements"].as_array().unwrap().len(), 1);
        assert_eq!(snap["currentRoute"], "/");
    }

    #[test]
    fn rejects_non_snapshot_json() {
        // A JSON error envelope with no `elements` must be rejected loudly,
        // not passed through as if it were a snapshot.
        let body = json!({ "error": "device offline", "code": "NO_DEVICE" }).to_string();
        let err = parse_snapshot_body(&body).unwrap_err();
        assert!(
            err.contains("elements"),
            "err names the missing field: {err}"
        );
    }

    #[test]
    fn rejects_html_error_page() {
        // A mis-routed tunnel can return an HTML 502 page; it must not parse
        // as a snapshot.
        let body = "<html><body>502 Bad Gateway</body></html>";
        let err = parse_snapshot_body(body).unwrap_err();
        assert!(err.contains("not JSON"), "err: {err}");
    }

    #[test]
    fn device_summary_projects_expected_fields() {
        let info = CloudDeviceInfo {
            device_id: "RFCW6041HHN".to_string(),
            display_name: "Pixel 7".to_string(),
            platform: "android".to_string(),
            app_id: "com.qontinui.mobile".to_string(),
            connected_since: "2026-06-20T10:00:00Z".to_string(),
            relay_session_id: "sess-1".to_string(),
        };
        let v = device_summary(&info);
        assert_eq!(v["deviceId"], "RFCW6041HHN");
        assert_eq!(v["displayName"], "Pixel 7");
        assert_eq!(v["platform"], "android");
        assert_eq!(v["appId"], "com.qontinui.mobile");
        // relay_session_id is intentionally NOT exposed.
        assert!(v.get("relaySessionId").is_none());
    }

    #[test]
    fn query_accepts_camel_and_snake_case() {
        // Exercise the serde rename/alias on the query struct without pulling
        // serde_urlencoded as a direct dev-dep (sdk_client.rs avoids it too).
        let camel: CloudSnapshotQuery =
            serde_json::from_value(json!({ "deviceId": "ABC123" })).expect("camelCase parses");
        assert_eq!(camel.device_id, "ABC123");
        let snake: CloudSnapshotQuery =
            serde_json::from_value(json!({ "device_id": "ABC123" })).expect("snake_case parses");
        assert_eq!(snake.device_id, "ABC123");
    }

    #[test]
    fn route_entries_match_route_count() {
        assert_eq!(route_entries().len(), 3);
        for (method, path) in route_entries() {
            assert_eq!(*method, "GET");
            assert!(path.starts_with("/ui-bridge/cloud-"));
        }
    }
}
