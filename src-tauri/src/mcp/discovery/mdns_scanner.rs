//! mDNS-based LAN device scanner.
//!
//! Devices running UI Bridge advertise themselves via mDNS under the service
//! type `_uibridge._tcp.local.`  This module consumes those advertisements
//! and emits `MdnsEvent`s so the physical device registry can add LAN
//! transports automatically.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ============================================================================
// Constants
// ============================================================================

/// mDNS service type advertised by UI Bridge instances on the local network.
pub const MDNS_SERVICE_TYPE: &str = "_uibridge._tcp.local.";

/// Plan 1C: the runner also advertises itself under this service type so phones
/// can find the runner. Separate from the device-side `_uibridge._tcp.local.`
/// so mutual discovery doesn't cause a feedback loop.
pub const RUNNER_MDNS_SERVICE_TYPE: &str = "_qontinui._tcp.local.";

// ============================================================================
// Types
// ============================================================================

/// Metadata about a device discovered via mDNS.
#[derive(Debug, Clone)]
pub struct MdnsDeviceInfo {
    pub device_id: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub txt_records: HashMap<String, String>,
}

/// Events emitted by the mDNS scanner.
#[derive(Debug, Clone)]
pub enum MdnsEvent {
    /// A new device appeared on the local network.
    Discovered(MdnsDeviceInfo),
    /// A previously discovered device is no longer reachable.
    Removed(String),
}

// ============================================================================
// MdnsScanner
// ============================================================================

/// Scans the local network for UI Bridge services via mDNS.
pub struct MdnsScanner {
    daemon: Option<ServiceDaemon>,
}

impl MdnsScanner {
    /// Create a new scanner.  If the mDNS daemon cannot be initialised (e.g.
    /// WinSock2 not available), the scanner operates in no-op mode and logs a
    /// warning.
    pub fn new() -> Self {
        match ServiceDaemon::new() {
            Ok(daemon) => Self {
                daemon: Some(daemon),
            },
            Err(e) => {
                warn!(
                    "Failed to create mDNS daemon: {}. LAN discovery disabled.",
                    e
                );
                Self { daemon: None }
            }
        }
    }

    /// Start scanning for UI Bridge services on the local network.
    ///
    /// Sends [`MdnsEvent`]s through `event_tx` as devices appear and
    /// disappear.  Events are delivered from a dedicated OS thread so the
    /// receiver must handle them from an async context (e.g. via
    /// `mpsc::Receiver::recv().await`).
    ///
    /// If the mDNS daemon is not available the method logs a message and
    /// returns immediately without spawning a thread.
    pub fn start(&self, event_tx: mpsc::Sender<MdnsEvent>) {
        let daemon = match &self.daemon {
            Some(d) => d,
            None => {
                info!(
                    "mDNS daemon not available, skipping LAN discovery. \
                     Use POST /ui-bridge/devices/register-lan to add LAN devices manually."
                );
                return;
            }
        };

        let receiver = match daemon.browse(MDNS_SERVICE_TYPE) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Failed to start mDNS browse for {}: {}",
                    MDNS_SERVICE_TYPE, e
                );
                return;
            }
        };

        info!("mDNS scanner started, browsing for {}", MDNS_SERVICE_TYPE);

        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let txt = info.get_properties();
                        let mut records: HashMap<String, String> = HashMap::new();
                        for prop in txt.iter() {
                            records.insert(prop.key().to_string(), prop.val_str().to_string());
                        }

                        let device_id = records
                            .get("device_id")
                            .cloned()
                            .unwrap_or_else(|| info.get_fullname().to_string());

                        let addresses: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                        let port = info.get_port();

                        debug!("mDNS discovered: {} at {:?}:{}", device_id, addresses, port);

                        let device_info = MdnsDeviceInfo {
                            device_id,
                            addresses,
                            port,
                            txt_records: records,
                        };

                        if event_tx
                            .blocking_send(MdnsEvent::Discovered(device_info))
                            .is_err()
                        {
                            break; // Receiver dropped — stop scanning
                        }
                    }
                    ServiceEvent::ServiceRemoved(_ty, fullname) => {
                        debug!("mDNS service removed: {}", fullname);
                        if event_tx
                            .blocking_send(MdnsEvent::Removed(fullname))
                            .is_err()
                        {
                            break;
                        }
                    }
                    ServiceEvent::SearchStarted(_) => {
                        debug!("mDNS search started for {}", MDNS_SERVICE_TYPE);
                    }
                    ServiceEvent::SearchStopped(_) => {
                        debug!("mDNS search stopped for {}", MDNS_SERVICE_TYPE);
                    }
                    _ => {}
                }
            }

            debug!("mDNS scanner thread exiting");
        });
    }

    /// Stop browsing for the UI Bridge service type.
    pub fn stop(&self) {
        if let Some(daemon) = &self.daemon {
            if let Err(e) = daemon.stop_browse(MDNS_SERVICE_TYPE) {
                debug!("mDNS stop_browse returned: {}", e);
            }
        }
    }
}

impl MdnsScanner {
    /// Plan 1C: register the runner itself under `RUNNER_MDNS_SERVICE_TYPE` so
    /// phones on the same LAN can discover it.
    ///
    /// `instance_name` should be unique per runner (hostname or machine id).
    /// `port` is the MCP API port the runner is listening on.
    /// `properties` become TXT records (device_id, version, auth_mode, …).
    pub fn register_runner(
        &self,
        instance_name: &str,
        port: u16,
        properties: HashMap<String, String>,
    ) -> Result<(), String> {
        let daemon = self.daemon.as_ref().ok_or("mDNS daemon unavailable")?;

        // ServiceInfo::new expects host IPs; use local hostname so clients resolve.
        let host = hostname::get()
            .ok()
            .and_then(|h| h.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "qontinui".to_string());
        let host = format!("{}.local.", host);

        let svc = ServiceInfo::new(
            RUNNER_MDNS_SERVICE_TYPE,
            instance_name,
            &host,
            "",
            port,
            properties,
        )
        .map_err(|e| format!("invalid ServiceInfo: {e}"))?
        .enable_addr_auto();

        daemon
            .register(svc)
            .map_err(|e| format!("mDNS register failed: {e}"))?;
        info!(
            service = RUNNER_MDNS_SERVICE_TYPE,
            instance = instance_name,
            port,
            "runner registered under mDNS"
        );
        Ok(())
    }

    /// Unregister the runner's own advertisement. Best-effort.
    pub fn unregister_runner(&self, instance_name: &str) {
        if let Some(daemon) = &self.daemon {
            let fullname = format!("{}.{}", instance_name, RUNNER_MDNS_SERVICE_TYPE);
            if let Err(e) = daemon.unregister(&fullname) {
                debug!(error = %e, "mDNS unregister returned");
            }
        }
    }

    /// Plan 1C: start scanning and fan out every event as a Tauri event so the
    /// wizard's React UI can update in real-time.
    ///
    /// Events emitted to the frontend:
    /// - `device-discovered` — payload is [`MdnsDeviceEventPayload`]
    /// - `device-removed` — payload is `{ fullname }`
    pub fn start_with_tauri_events(&self, app: tauri::AppHandle) {
        let (tx, mut rx) = mpsc::channel::<MdnsEvent>(64);
        self.start(tx);

        tokio::spawn(async move {
            use tauri::Emitter;
            while let Some(ev) = rx.recv().await {
                match ev {
                    MdnsEvent::Discovered(info) => {
                        let payload = MdnsDeviceEventPayload {
                            device_id: info.device_id,
                            port: info.port,
                            addresses: info.addresses.iter().map(|a| a.to_string()).collect(),
                            txt_records: info.txt_records,
                        };
                        let _ = app.emit("device-discovered", &payload);
                    }
                    MdnsEvent::Removed(fullname) => {
                        let _ = app.emit(
                            "device-removed",
                            serde_json::json!({
                                "fullname": fullname,
                            }),
                        );
                    }
                }
            }
            debug!("mDNS Tauri event bridge ended");
        });
    }
}

/// Payload shape emitted to the frontend on `device-discovered`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MdnsDeviceEventPayload {
    pub device_id: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub txt_records: HashMap<String, String>,
}

impl Default for MdnsScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MdnsScanner {
    fn drop(&mut self) {
        self.stop();
        if let Some(daemon) = self.daemon.take() {
            if let Err(e) = daemon.shutdown() {
                debug!("mDNS daemon shutdown returned: {}", e);
            }
        }
    }
}
