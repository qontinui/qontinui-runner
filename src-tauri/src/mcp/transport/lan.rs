//! LAN transport for physical device connections over Wi-Fi.
//!
//! Discovers devices via mDNS or manual IP entry, validates them via HTTP
//! health check, then starts a local TCP proxy so the device's UI Bridge
//! HTTP server appears on `127.0.0.1:{port}` — matching the USB/Cloud pattern.

use std::net::{IpAddr, SocketAddr};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use super::TransportError;
use crate::mcp::adb_helper;
use crate::mcp::app_discovery::DiscoveredApp;

// ============================================================================
// LanTransport
// ============================================================================

/// Handles LAN (Wi-Fi) connections to physical devices.
pub struct LanTransport;

impl LanTransport {
    pub fn new() -> Self {
        Self
    }

    /// Validate that the device at `address` is reachable and has UI Bridge
    /// running.  Returns a `DiscoveredApp` with the device's metadata.
    pub async fn check_device(&self, address: SocketAddr) -> Result<DiscoveredApp, TransportError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| TransportError::HealthCheckFailed {
                address: address.to_string(),
                reason: e.to_string(),
            })?;

        let url = format!("http://{}/ui-bridge/health", address);

        let resp =
            client
                .get(&url)
                .send()
                .await
                .map_err(|e| TransportError::DeviceUnreachable {
                    address: address.to_string(),
                })?;

        let body: serde_json::Value =
            resp.json()
                .await
                .map_err(|e| TransportError::HealthCheckFailed {
                    address: address.to_string(),
                    reason: format!("invalid JSON: {}", e),
                })?;

        let ui_bridge = body
            .get("uiBridge")
            .ok_or_else(|| TransportError::HealthCheckFailed {
                address: address.to_string(),
                reason: "response missing 'uiBridge' field".to_string(),
            })?;

        let now = chrono::Utc::now().timestamp_millis();

        Ok(DiscoveredApp {
            app_id: ui_bridge
                .get("appId")
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("lan-{}", address.port()))
                .to_string(),
            app_name: ui_bridge
                .get("appName")
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("Device at {}", address.ip()))
                .to_string(),
            app_type: ui_bridge
                .get("appType")
                .and_then(|v| v.as_str())
                .unwrap_or("mobile")
                .to_string(),
            framework: ui_bridge
                .get("framework")
                .and_then(|v| v.as_str())
                .map(String::from),
            url: format!("http://{}", address),
            port: address.port(),
            base_path: "/ui-bridge".to_string(),
            version: ui_bridge
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from),
            capabilities: ui_bridge
                .get("capabilities")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            element_count: None,
            component_count: None,
            discovered_at: now,
        })
    }

    /// Start a local TCP proxy that forwards all connections from
    /// `127.0.0.1:0` to the remote `address`.
    ///
    /// Returns `(local_port, join_handle)`.  The join handle runs the accept
    /// loop; dropping it or awaiting it ends the proxy.
    pub async fn create_proxy(
        &self,
        address: SocketAddr,
    ) -> Result<(u16, tokio::task::JoinHandle<()>), TransportError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| TransportError::ProxyBindFailed(e.to_string()))?;

        let local_port = listener
            .local_addr()
            .map_err(|e| TransportError::ProxyBindFailed(e.to_string()))?
            .port();

        info!(
            local_port,
            remote = %address,
            "LAN TCP proxy started"
        );

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((local_stream, peer_addr)) => {
                        debug!(peer = %peer_addr, remote = %address, "LAN proxy: new connection");
                        tokio::spawn(proxy_connection(local_stream, address));
                    }
                    Err(e) => {
                        warn!(error = %e, "LAN proxy: accept error, stopping");
                        break;
                    }
                }
            }
        });

        Ok((local_port, handle))
    }
}

impl Default for LanTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Plan 1A: enable ADB TCP/IP mode on a USB-connected device and return the
/// `{ip}:{port}` address the runner can reach it on over Wi-Fi.
///
/// Steps:
/// 1. `setprop service.adb.tcp.port {port}` on the device
/// 2. restart adbd (`stop adbd` + `start adbd`)
/// 3. discover the device's Wi-Fi IPv4 via `ip route` / `ip addr`
///
/// The caller should wait ~2-3 seconds after the restart before trying to connect,
/// since adbd takes a moment to re-bind on the new port.
pub async fn enable_tcpip_for_device(
    adb_serial: &str,
    port: u16,
) -> Result<SocketAddr, TransportError> {
    adb_helper::enable_tcpip(adb_serial.to_string(), port)
        .await
        .map_err(TransportError::AdbCommandFailed)?;

    let ip_str = adb_helper::get_wlan_ipv4(adb_serial.to_string())
        .await
        .map_err(TransportError::AdbCommandFailed)?
        .ok_or_else(|| {
            TransportError::AdbCommandFailed(format!(
                "device {adb_serial} has no IPv4 address on wlan0 (is Wi-Fi connected?)"
            ))
        })?;

    let ip: IpAddr = ip_str
        .parse()
        .map_err(|e| TransportError::AdbCommandFailed(format!("invalid IPv4 {ip_str}: {e}")))?;

    Ok(SocketAddr::new(ip, port))
}

/// Bidirectionally copy data between the incoming local connection and the
/// remote device connection.
async fn proxy_connection(mut local: TcpStream, remote_addr: SocketAddr) {
    let mut remote = match TcpStream::connect(remote_addr).await {
        Ok(s) => s,
        Err(e) => {
            debug!(remote = %remote_addr, error = %e, "LAN proxy: failed to connect to remote");
            return;
        }
    };

    match tokio::io::copy_bidirectional(&mut local, &mut remote).await {
        Ok((sent, recv)) => {
            debug!(sent, recv, remote = %remote_addr, "LAN proxy: connection closed");
        }
        Err(e) => {
            debug!(error = %e, remote = %remote_addr, "LAN proxy: connection error");
        }
    }
}
