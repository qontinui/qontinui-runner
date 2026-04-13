//! mDNS-based LAN device scanner.
//!
//! Devices running UI Bridge advertise themselves via mDNS under the service
//! type `_uibridge._tcp.local.`  This module consumes those advertisements
//! and emits `MdnsEvent`s so the physical device registry can add LAN
//! transports automatically.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ============================================================================
// Constants
// ============================================================================

/// mDNS service type advertised by UI Bridge instances on the local network.
pub const MDNS_SERVICE_TYPE: &str = "_uibridge._tcp.local.";

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
                warn!("Failed to start mDNS browse for {}: {}", MDNS_SERVICE_TYPE, e);
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
                            records.insert(
                                prop.key().to_string(),
                                prop.val_str().to_string(),
                            );
                        }

                        let device_id = records
                            .get("device_id")
                            .cloned()
                            .unwrap_or_else(|| info.get_fullname().to_string());

                        let addresses: Vec<IpAddr> =
                            info.get_addresses().iter().copied().collect();
                        let port = info.get_port();

                        debug!(
                            "mDNS discovered: {} at {:?}:{}",
                            device_id, addresses, port
                        );

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
