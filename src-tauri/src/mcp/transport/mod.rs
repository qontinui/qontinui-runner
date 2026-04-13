//! Transport layer for physical device UI Bridge connections.
//!
//! Supports three transport kinds:
//! - USB — ADB port forwarding (highest priority, lowest latency)
//! - LAN — Direct Wi-Fi connection via TCP proxy
//! - Cloud — Relayed via qontinui.io backend WebSocket (fallback)

pub mod lan;
pub mod pairing;
pub mod usb;
// Cloud transport is a future phase — module declared but empty for now
pub mod cloud;

use serde::{Deserialize, Serialize};
use std::fmt;

// ============================================================================
// TransportKind
// ============================================================================

/// The underlying transport mechanism used to reach a physical device.
///
/// Priority order: USB (0) > LAN (1) > Cloud (2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind {
    Usb,
    Lan,
    Cloud,
}

impl TransportKind {
    /// Lower value = higher priority (USB wins over LAN wins over Cloud).
    pub fn priority(&self) -> u8 {
        match self {
            TransportKind::Usb => 0,
            TransportKind::Lan => 1,
            TransportKind::Cloud => 2,
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportKind::Usb => write!(f, "USB"),
            TransportKind::Lan => write!(f, "LAN"),
            TransportKind::Cloud => write!(f, "Cloud"),
        }
    }
}

// ============================================================================
// DeviceOs
// ============================================================================

/// Operating system of the physical device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceOs {
    Android,
    Ios,
}

impl fmt::Display for DeviceOs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceOs::Android => write!(f, "Android"),
            DeviceOs::Ios => write!(f, "iOS"),
        }
    }
}

// ============================================================================
// TransportError
// ============================================================================

/// Errors that can arise when establishing or using a transport.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("ADB not found or not executable: {0}")]
    AdbNotFound(String),

    #[error("ADB command failed: {0}")]
    AdbCommandFailed(String),

    #[error("Port forwarding failed for device {device_id}: {reason}")]
    ForwardFailed { device_id: String, reason: String },

    #[error("TCP proxy bind failed: {0}")]
    ProxyBindFailed(String),

    #[error("Health check failed for {address}: {reason}")]
    HealthCheckFailed { address: String, reason: String },

    #[error("Device not reachable at {address}")]
    DeviceUnreachable { address: String },

    #[error("Pairing error: {0}")]
    PairingError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
