//! Cloud registry poller — discovers devices registered with the qontinui.io relay backend.
//!
//! Polls `GET /api/v1/device-bridge/available-devices` at a configurable interval,
//! then sends the results through an async channel for consumption by the device registry.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

// ============================================================================
// Types
// ============================================================================

/// Information about a cloud-registered device returned by the relay backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudDeviceInfo {
    pub device_id: String,
    pub display_name: String,
    pub platform: String,
    pub app_id: String,
    pub connected_since: String,
    pub relay_session_id: String,
}

// ============================================================================
// CloudRegistryPoller
// ============================================================================

/// Polls the qontinui.io backend for cloud-registered devices.
pub struct CloudRegistryPoller {
    backend_url: String,
    poll_interval: Duration,
}

impl CloudRegistryPoller {
    /// Create a new poller.
    ///
    /// * `backend_url` — base URL of the qontinui.io backend (e.g. `https://api.qontinui.io`)
    /// * `poll_interval_secs` — how often to poll, in seconds
    pub fn new(backend_url: String, poll_interval_secs: u64) -> Self {
        Self {
            backend_url,
            poll_interval: Duration::from_secs(poll_interval_secs),
        }
    }

    /// Perform a single poll against the backend.
    ///
    /// Issues `GET {backend_url}/api/v1/device-bridge/available-devices` with a
    /// `Bearer {auth_token}` header, then parses the JSON response as
    /// `Vec<CloudDeviceInfo>`.
    pub async fn poll(&self, auth_token: &str) -> Result<Vec<CloudDeviceInfo>, reqwest::Error> {
        let url = format!(
            "{}/api/v1/device-bridge/available-devices",
            self.backend_url.trim_end_matches('/')
        );

        debug!("[cloud-registry] Polling {}", url);

        let client = reqwest::Client::new();
        let resp = client.get(&url).bearer_auth(auth_token).send().await?;

        // Treat non-2xx as an error via response error handling
        let resp = resp.error_for_status()?;

        // The backend wraps the list in `{ "devices": [...] }` or returns the
        // array directly. Try to unwrap either form.
        let json: serde_json::Value = resp.json().await?;

        let devices = if let Some(arr) = json.as_array() {
            serde_json::from_value(serde_json::Value::Array(arr.clone())).unwrap_or_default()
        } else if let Some(arr) = json.get("devices").and_then(|v| v.as_array()) {
            serde_json::from_value(serde_json::Value::Array(arr.clone())).unwrap_or_default()
        } else {
            Vec::new()
        };

        debug!("[cloud-registry] Found {} cloud device(s)", devices.len());

        Ok(devices)
    }

    /// Start a background polling task.
    ///
    /// * `auth_token_provider` — called before each poll to retrieve the current auth token.
    ///   If it returns `None` the poll is skipped for that cycle.
    /// * `event_tx` — channel where polled results are sent.
    /// * `shutdown` — watch receiver; task exits when the value becomes `true`.
    pub fn start_polling(
        self,
        auth_token_provider: impl Fn() -> Option<String> + Send + 'static,
        event_tx: mpsc::Sender<Vec<CloudDeviceInfo>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tokio::spawn(async move {
            info!(
                "[cloud-registry] Background poller started (interval {:?})",
                self.poll_interval
            );

            loop {
                // Check for shutdown first
                if *shutdown.borrow() {
                    info!("[cloud-registry] Shutdown flag set — stopping poller");
                    break;
                }

                // Retrieve token
                if let Some(token) = auth_token_provider() {
                    match self.poll(&token).await {
                        Ok(devices) => {
                            if event_tx.send(devices).await.is_err() {
                                warn!("[cloud-registry] Event channel closed — stopping poller");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("[cloud-registry] Poll error: {e}");
                        }
                    }
                } else {
                    debug!("[cloud-registry] No auth token available, skipping poll");
                }

                // Wait for the next interval or a shutdown signal
                tokio::select! {
                    _ = tokio::time::sleep(self.poll_interval) => {}
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("[cloud-registry] Shutdown signal received — stopping poller");
                            break;
                        }
                    }
                }
            }

            info!("[cloud-registry] Background poller stopped");
        });
    }
}

// ============================================================================
// The supervised registry poller
// ============================================================================

/// The `worker_supervisor` registry name of the loop [`spawn_registry_poller`]
/// starts, as `GET /health` `supervised_workers` lists it. Absent from `/health`
/// when the cloud device bridge is off: the gate sits before the spawn.
pub const CLOUD_REGISTRY_POLLER_WORKER: &str = "cloud_registry.poller";

/// How long the poller waits before its first poll.
pub const CLOUD_REGISTRY_POLLER_START_DELAY: Duration = Duration::from_secs(8);

/// What a run of the poller needs from `CloudRelaySettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPollerConfig {
    pub backend_url: String,
    /// Seconds between polls, at least 1: `tokio::time::interval` panics on a
    /// zero period, and under the supervisor that panic would be a restart
    /// every backoff instead of a poller.
    pub poll_secs: u64,
}

/// The gate: `None` when the cloud device bridge is disabled, so no worker is
/// started (and none is listed on `/health`); otherwise the poller's config.
pub fn registry_poller_config(
    settings: &crate::settings::CloudRelaySettings,
) -> Option<RegistryPollerConfig> {
    if !settings.device_bridge_enabled {
        return None;
    }
    Some(RegistryPollerConfig {
        backend_url: settings.backend_url.clone(),
        poll_secs: settings.cloud_registry_poll_secs.max(1),
    })
}

/// Poll the qontinui.io backend for remotely registered devices and open a
/// cloud tunnel for each one the registry does not already reach over the
/// cloud — when `settings` enables the cloud device bridge. Returns `None`,
/// starting nothing, when it does not.
///
/// The [`CloudTransport`](crate::mcp::transport::cloud::CloudTransport) is built
/// once, here, before the spawn: its tunnel map owns every open tunnel, and
/// dropping a handle closes its tunnel. A supervisor restart therefore rebuilds
/// only the loop; the rebuilt run shares the transport, so the tunnels the
/// registry points at stay open, and the run's Cloud-transport dedup stays true.
pub fn spawn_registry_poller(
    registry: std::sync::Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    settings: &crate::settings::CloudRelaySettings,
    start_delay: Duration,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(config) = registry_poller_config(settings) else {
        debug!("Cloud device bridge disabled, skipping registry poller");
        return None;
    };
    // Use localhost for the tunnel WS connection — runner is co-located
    // with the backend. The tunnel URL (Cloudflare) may reject WS upgrades.
    let transport = std::sync::Arc::new(crate::mcp::transport::cloud::CloudTransport::new(
        crate::api_config::get_api_base_url(),
    ));
    let factory = poller_factory(transport, registry, config, start_delay);
    Some(crate::worker_supervisor::spawn_supervised_with_heartbeat(
        CLOUD_REGISTRY_POLLER_WORKER,
        move |hb| factory(hb).run(),
    ))
}

/// The supervisor's factory: each call builds one run around the SAME
/// transport.
fn poller_factory(
    transport: std::sync::Arc<crate::mcp::transport::cloud::CloudTransport>,
    registry: std::sync::Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    config: RegistryPollerConfig,
    start_delay: Duration,
) -> impl Fn(crate::worker_supervisor::Heartbeat) -> PollerRun + Send + 'static {
    move |hb| PollerRun {
        transport: transport.clone(),
        registry: registry.clone(),
        config: config.clone(),
        start_delay,
        hb,
    }
}

/// One run of the poller loop.
struct PollerRun {
    transport: std::sync::Arc<crate::mcp::transport::cloud::CloudTransport>,
    registry: std::sync::Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    config: RegistryPollerConfig,
    start_delay: Duration,
    hb: crate::worker_supervisor::Heartbeat,
}

impl PollerRun {
    async fn run(self) {
        let PollerRun {
            transport: cloud_transport,
            registry,
            config,
            start_delay,
            hb,
        } = self;

        tokio::time::sleep(start_delay).await;

        let poller = CloudRegistryPoller::new(config.backend_url.clone(), config.poll_secs);
        let mut interval = tokio::time::interval(Duration::from_secs(config.poll_secs));

        loop {
            interval.tick().await;
            hb.tick();

            // Refresh auth token on each poll cycle
            let token = crate::auth::AuthManager::new().get_access_token().ok();
            let Some(token) = token else { continue };
            match poller.poll(&token).await {
                Ok(devices) => {
                    for device in devices {
                        debug!(
                            "Cloud device available: {} ({})",
                            device.device_id, device.display_name
                        );
                        // Skip a device that already has a cloud tunnel. One known
                        // only over LAN or USB still gets one: a phone on Wi-Fi that
                        // requires pairing this runner has not done is reached over
                        // the relay instead, which its pairing gate does not cover.
                        if registry
                            .has_transport_kind(
                                &device.device_id,
                                crate::mcp::transport::TransportKind::Cloud,
                            )
                            .await
                        {
                            continue;
                        }
                        // Open a cloud tunnel for this device so the registry
                        // has a working proxy URL. The tunnel stays open for
                        // the device's lifetime in the registry.
                        match cloud_transport.open_tunnel(&device.device_id, &token).await {
                            Ok(local_port) => {
                                let os = match device.platform.to_lowercase().as_str() {
                                    "ios" => crate::mcp::transport::DeviceOs::Ios,
                                    _ => crate::mcp::transport::DeviceOs::Android,
                                };
                                let now = chrono::Utc::now().timestamp_millis();
                                let info = crate::mcp::physical_device::PhysicalDeviceInfo {
                                    id: device.device_id.clone(),
                                    os,
                                    device_kind: "physical".to_string(),
                                    model: None,
                                    app_id: if device.app_id.is_empty() {
                                        None
                                    } else {
                                        Some(device.app_id.clone())
                                    },
                                    ui_bridge_version: None,
                                    first_seen_at: now,
                                };
                                let transport = crate::mcp::physical_device::ActiveTransport {
                                    kind: crate::mcp::transport::TransportKind::Cloud,
                                    proxy_url: format!("http://127.0.0.1:{}", local_port),
                                    established_at: now,
                                    last_healthy_at: now,
                                    fail_count: 0,
                                };
                                registry.register(info, transport).await;
                                info!(
                                    "Cloud device registered: {} via tunnel on port {}",
                                    device.device_id, local_port
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to open cloud tunnel for {}: {}",
                                    device.device_id, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Cloud registry poll failed: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::physical_device::PhysicalDeviceRegistry;
    use crate::settings::CloudRelaySettings;
    use std::sync::Arc;

    fn enabled(poll_secs: u64) -> CloudRelaySettings {
        CloudRelaySettings {
            device_bridge_enabled: true,
            cloud_registry_poll_secs: poll_secs,
            ..CloudRelaySettings::default()
        }
    }

    #[test]
    fn the_gate_starts_nothing_when_the_device_bridge_is_disabled() {
        let settings = CloudRelaySettings {
            device_bridge_enabled: false,
            ..CloudRelaySettings::default()
        };
        assert_eq!(registry_poller_config(&settings), None);
    }

    #[test]
    fn the_gate_passes_the_settings_through_when_the_device_bridge_is_enabled() {
        let settings = enabled(45);
        assert_eq!(
            registry_poller_config(&settings),
            Some(RegistryPollerConfig {
                backend_url: settings.backend_url.clone(),
                poll_secs: 45,
            })
        );
    }

    /// `interval(0s)` panics; a zero setting is clamped to one second.
    #[test]
    fn the_gate_clamps_a_zero_poll_interval() {
        let config = registry_poller_config(&enabled(0)).expect("enabled");
        assert_eq!(config.poll_secs, 1);
    }

    /// The disabled door returns before spawning, so nothing is registered.
    #[tokio::test]
    async fn spawn_registry_poller_returns_none_when_disabled() {
        let settings = CloudRelaySettings::default();
        assert!(!settings.device_bridge_enabled);
        assert!(spawn_registry_poller(
            Arc::new(PhysicalDeviceRegistry::new()),
            &settings,
            CLOUD_REGISTRY_POLLER_START_DELAY,
        )
        .is_none());
    }

    /// What a supervisor restart does: call the factory a second time. Both
    /// runs hold the one transport whose tunnel map owns the open tunnels.
    #[test]
    fn two_factory_runs_share_the_cloud_transport() {
        let transport = Arc::new(crate::mcp::transport::cloud::CloudTransport::new(
            "http://127.0.0.1:9".to_string(),
        ));
        let factory = poller_factory(
            transport.clone(),
            Arc::new(PhysicalDeviceRegistry::new()),
            registry_poller_config(&enabled(30)).expect("enabled"),
            CLOUD_REGISTRY_POLLER_START_DELAY,
        );
        let first = factory(crate::worker_supervisor::Heartbeat(
            CLOUD_REGISTRY_POLLER_WORKER,
        ));
        let second = factory(crate::worker_supervisor::Heartbeat(
            CLOUD_REGISTRY_POLLER_WORKER,
        ));
        assert!(Arc::ptr_eq(&first.transport, &second.transport));
        assert!(Arc::ptr_eq(&first.transport, &transport));
    }

    /// The enabled door registers the poller with the panic supervisor under its
    /// `/health` name. The start delay is long, so no poll leaves the process.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_registry_poller_registers_with_the_panic_supervisor_when_enabled() {
        let handle = spawn_registry_poller(
            Arc::new(PhysicalDeviceRegistry::new()),
            &enabled(30),
            Duration::from_secs(3600),
        )
        .expect("an enabled bridge starts the poller");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !crate::worker_supervisor::snapshot()
            .iter()
            .any(|row| row.name == CLOUD_REGISTRY_POLLER_WORKER)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "spawn_registry_poller never registered {CLOUD_REGISTRY_POLLER_WORKER}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.abort();
    }
}
