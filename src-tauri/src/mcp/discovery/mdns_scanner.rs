//! mDNS-based LAN device scanner.
//!
//! Devices running UI Bridge advertise themselves via mDNS under the service
//! type `_ui-bridge._tcp.local`.  This module will consume those advertisements
//! and emit `MdnsEvent`s so the physical device registry can add LAN transports
//! automatically.
//!
//! ## Current status
//!
//! The `mdns-sd` crate requires verification on Windows (Blackwell/WinSock2
//! compatibility) before it can be added to the lockfile.  Until that is
//! confirmed, the scanner logs a startup message and does nothing — manual
//! LAN device registration via `POST /ui-bridge/devices/register-lan` is the
//! supported path.

use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::info;

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

pub struct MdnsScanner;

impl MdnsScanner {
    pub fn new() -> Self {
        Self
    }

    /// Start the mDNS scanner.  Sends events through `event_tx` as devices
    /// appear and disappear.
    ///
    /// This is currently a no-op stub that logs a startup message.  Real
    /// `mdns-sd` integration will be added once Windows support is confirmed.
    pub fn start(&self, _event_tx: mpsc::Sender<MdnsEvent>) {
        info!(
            "mDNS scanner started (mdns-sd integration pending). \
             Use POST /ui-bridge/devices/register-lan to add LAN devices manually."
        );
    }
}

impl Default for MdnsScanner {
    fn default() -> Self {
        Self::new()
    }
}
