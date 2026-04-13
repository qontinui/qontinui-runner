//! Physical Device Registry
//!
//! Tracks physical mobile devices (Android/iOS) that have connected to this
//! runner via USB, LAN, or Cloud transports.  Each device may have multiple
//! active transports; the highest-priority one (USB > LAN > Cloud) is used
//! as the "active" proxy URL for SDK requests.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::mcp::transport::{DeviceOs, TransportKind};

// ============================================================================
// Types
// ============================================================================

pub type PhysicalDeviceId = String;

/// Static metadata about a physical device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalDeviceInfo {
    pub id: PhysicalDeviceId,
    pub os: DeviceOs,
    /// "physical" or "emulator"
    pub device_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// App bundle/package ID running UI Bridge on the device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_bridge_version: Option<String>,
    /// Unix timestamp (ms) when the device was first seen.
    pub first_seen_at: i64,
    /// Opaque token used to authenticate LAN/Cloud transports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing_token: Option<String>,
}

/// A single established transport to a device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTransport {
    pub kind: TransportKind,
    /// Always `http://127.0.0.1:{port}` — even for LAN, which runs a local
    /// TCP proxy so the caller never needs to know the real device IP.
    pub proxy_url: String,
    /// Unix timestamp (ms) when this transport was established.
    pub established_at: i64,
    /// Unix timestamp (ms) of the last successful health check.
    pub last_healthy_at: i64,
    /// Consecutive health-check failures.
    pub fail_count: u32,
}

/// Coarse health classification used by the registry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthState {
    Healthy,
    Degraded(String),
    Unreachable,
}

/// A device entry in the registry, combining static info + live transports.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalDevice {
    pub info: PhysicalDeviceInfo,
    /// Transports sorted by priority (USB first).
    pub transports: Vec<ActiveTransport>,
    /// Index into `transports` for the currently active transport.
    pub active_transport_idx: usize,
    pub health_state: HealthState,
}

impl PhysicalDevice {
    /// Returns the proxy URL of the currently active transport, if any.
    pub fn active_proxy_url(&self) -> Option<&str> {
        self.transports
            .get(self.active_transport_idx)
            .map(|t| t.proxy_url.as_str())
    }
}

// ============================================================================
// PhysicalDeviceRegistry
// ============================================================================

/// Thread-safe registry of all known physical devices.
pub struct PhysicalDeviceRegistry {
    devices: Arc<RwLock<HashMap<PhysicalDeviceId, PhysicalDevice>>>,
}

impl PhysicalDeviceRegistry {
    pub fn new() -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // -----------------------------------------------------------------------
    // Mutations
    // -----------------------------------------------------------------------

    /// Register a device, or add a new transport to an existing entry.
    /// Transports are kept sorted by priority (lowest priority number first).
    pub async fn register(&self, info: PhysicalDeviceInfo, transport: ActiveTransport) {
        let mut map = self.devices.write().await;

        match map.get_mut(&info.id) {
            Some(device) => {
                // Remove existing transport of the same kind, then add the new one
                device
                    .transports
                    .retain(|t| t.kind != transport.kind);
                device.transports.push(transport);
                sort_transports(&mut device.transports);
                device.active_transport_idx = 0;
                device.health_state = HealthState::Healthy;
                info!(id = %device.info.id, "Updated existing device registration");
            }
            None => {
                let id = info.id.clone();
                let device = PhysicalDevice {
                    info,
                    transports: vec![transport],
                    active_transport_idx: 0,
                    health_state: HealthState::Healthy,
                };
                map.insert(id.clone(), device);
                info!(id = %id, "Registered new physical device");
            }
        }
    }

    /// Return the active transport's proxy URL for a device.
    pub async fn get_active_proxy_url(&self, id: &str) -> Option<String> {
        let map = self.devices.read().await;
        map.get(id)
            .and_then(|d| d.active_proxy_url().map(String::from))
    }

    /// Clone and return a device entry.
    pub async fn get_device(&self, id: &str) -> Option<PhysicalDevice> {
        let map = self.devices.read().await;
        map.get(id).cloned()
    }

    /// Clone and return all registered devices.
    pub async fn list_all(&self) -> Vec<PhysicalDevice> {
        let map = self.devices.read().await;
        map.values().cloned().collect()
    }

    /// Add or replace a transport of the given kind, then re-sort and reset
    /// the active index to 0 (best transport).
    pub async fn promote_transport(
        &self,
        id: &str,
        kind: TransportKind,
        proxy_url: String,
    ) {
        let mut map = self.devices.write().await;
        if let Some(device) = map.get_mut(id) {
            device.transports.retain(|t| t.kind != kind);
            let now = chrono::Utc::now().timestamp_millis();
            device.transports.push(ActiveTransport {
                kind,
                proxy_url,
                established_at: now,
                last_healthy_at: now,
                fail_count: 0,
            });
            sort_transports(&mut device.transports);
            device.active_transport_idx = 0;
            device.health_state = HealthState::Healthy;
            debug!(id, %kind, "Transport promoted");
        }
    }

    /// Remove a transport.  If the removed transport was active, fail over to
    /// the next best one.  If no transports remain, mark the device Unreachable.
    pub async fn remove_transport(&self, id: &str, kind: TransportKind) {
        let mut map = self.devices.write().await;
        if let Some(device) = map.get_mut(id) {
            device.transports.retain(|t| t.kind != kind);

            if device.transports.is_empty() {
                device.active_transport_idx = 0;
                device.health_state = HealthState::Unreachable;
                warn!(id, %kind, "All transports removed — device unreachable");
            } else {
                device.active_transport_idx = 0; // best remaining transport
                device.health_state = HealthState::Healthy;
                debug!(id, %kind, "Transport removed, failing over to next best");
            }
        }
    }

    /// Remove a device entirely from the registry.
    pub async fn remove_device(&self, id: &str) {
        let mut map = self.devices.write().await;
        map.remove(id);
        info!(id, "Physical device removed from registry");
    }

    // -----------------------------------------------------------------------
    // Health Monitor
    // -----------------------------------------------------------------------

    /// Spawn a background task that checks `GET {proxy_url}/ui-bridge/health`
    /// every 15 seconds for every registered device's active transport.
    ///
    /// On three consecutive failures the transport is removed and the next
    /// best transport is promoted.  If no transports survive the device is
    /// marked `Unreachable`.
    pub fn start_health_monitor(
        self: Arc<Self>,
        client: reqwest::Client,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("Physical device health monitor shutting down");
                            break;
                        }
                    }
                }

                // Collect (device_id, transport_kind, proxy_url) for active transports
                let targets: Vec<(String, TransportKind, String)> = {
                    let map = self.devices.read().await;
                    map.values()
                        .filter_map(|d| {
                            d.transports.get(d.active_transport_idx).map(|t| {
                                (d.info.id.clone(), t.kind, t.proxy_url.clone())
                            })
                        })
                        .collect()
                };

                for (id, kind, proxy_url) in targets {
                    let url = format!("{}/ui-bridge/health", proxy_url);
                    let ok = match tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        client.get(&url).send(),
                    )
                    .await
                    {
                        Ok(Ok(resp)) => resp.status().is_success(),
                        _ => false,
                    };

                    let mut map = self.devices.write().await;
                    let device = match map.get_mut(&id) {
                        Some(d) => d,
                        None => continue,
                    };

                    // Find the transport in the device's list
                    let transport = match device
                        .transports
                        .iter_mut()
                        .find(|t| t.kind == kind)
                    {
                        Some(t) => t,
                        None => continue,
                    };

                    if ok {
                        transport.last_healthy_at = chrono::Utc::now().timestamp_millis();
                        transport.fail_count = 0;
                        device.health_state = HealthState::Healthy;
                    } else {
                        transport.fail_count += 1;
                        let fail_count = transport.fail_count;
                        warn!(
                            id = %id,
                            %kind,
                            fail_count,
                            "Health check failed for transport"
                        );

                        if fail_count >= 3 {
                            // Drop the write lock before calling remove_transport
                            drop(map);
                            self.remove_transport(&id, kind).await;
                        } else {
                            device.health_state =
                                HealthState::Degraded(format!("{} health check failed", kind));
                        }
                    }
                }
            }
        });
    }
}

impl Default for PhysicalDeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Sort transports by priority (lowest number = highest priority).
fn sort_transports(transports: &mut Vec<ActiveTransport>) {
    transports.sort_by_key(|t| t.kind.priority());
}
