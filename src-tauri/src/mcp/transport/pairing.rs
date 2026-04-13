//! Pairing manager for LAN and Cloud device authentication.
//!
//! LAN and Cloud transports require explicit pairing before a device can
//! connect, to prevent unauthorized devices from reaching the runner API.
//! USB is exempt (ADB authorization covers it).
//!
//! Pairing flow:
//! 1. Runner calls `initiate(device_id, transport)` → gets a 6-digit code.
//! 2. User enters the code on the mobile device.
//! 3. Device POSTs the code to `POST /ui-bridge/devices/pair/confirm`.
//! 4. Runner calls `confirm(device_id, code)` → issues a device_token.
//! 5. Subsequent requests from the device include the token in a header.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use super::TransportKind;

// ============================================================================
// Types
// ============================================================================

/// A pairing attempt that is waiting for code confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairing {
    pub device_id: String,
    /// 6-digit numeric code shown to the user.
    pub code: String,
    /// Unix timestamp (ms) when the pairing was initiated.
    pub created_at: i64,
    /// Unix timestamp (ms) after which the code is invalid.
    pub expires_at: i64,
    /// Which transport the device intends to use.
    pub transport_hint: TransportKind,
}

/// A successfully paired device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub device_id: String,
    /// Opaque 64-hex-character bearer token the device includes in requests.
    pub device_token: String,
    /// Unix timestamp (ms) when pairing was completed.
    pub paired_at: i64,
    /// Human-readable name supplied by the device (e.g., "Josh's Pixel 8").
    pub display_name: String,
    /// Platform string from the device (e.g., "android", "ios").
    pub platform: String,
}

/// Serializable version of `PairedDevice` for persisting in settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceRecord {
    pub device_id: String,
    pub device_token: String,
    pub paired_at: i64,
    pub display_name: String,
    pub platform: String,
}

impl From<PairedDevice> for PairedDeviceRecord {
    fn from(d: PairedDevice) -> Self {
        Self {
            device_id: d.device_id,
            device_token: d.device_token,
            paired_at: d.paired_at,
            display_name: d.display_name,
            platform: d.platform,
        }
    }
}

impl From<PairedDeviceRecord> for PairedDevice {
    fn from(r: PairedDeviceRecord) -> Self {
        Self {
            device_id: r.device_id,
            device_token: r.device_token,
            paired_at: r.paired_at,
            display_name: r.display_name,
            platform: r.platform,
        }
    }
}

/// Errors that can occur during pairing.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("No pending pairing found for device {0}")]
    NoPendingPairing(String),

    #[error("Pairing code is incorrect")]
    InvalidCode,

    #[error("Pairing code has expired")]
    Expired,
}

// ============================================================================
// PairingManager
// ============================================================================

pub struct PairingManager {
    pending: Arc<Mutex<HashMap<String, PendingPairing>>>,
    paired: Arc<RwLock<Vec<PairedDevice>>>,
}

impl PairingManager {
    /// Create an empty pairing manager.
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            paired: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Initialize from persisted settings records.
    pub fn load_paired(records: Vec<PairedDeviceRecord>) -> Self {
        let devices: Vec<PairedDevice> = records.into_iter().map(PairedDevice::from).collect();
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            paired: Arc::new(RwLock::new(devices)),
        }
    }

    /// Begin a pairing for the given device.  Generates a 6-digit code with a
    /// 10-minute expiry.  If a pending pairing already exists, it is replaced.
    pub async fn initiate(&self, device_id: &str, transport: TransportKind) -> PendingPairing {
        let now = chrono::Utc::now().timestamp_millis();
        let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
        let pairing = PendingPairing {
            device_id: device_id.to_string(),
            code,
            created_at: now,
            expires_at: now + 10 * 60 * 1000, // 10 minutes
            transport_hint: transport,
        };

        let mut pending = self.pending.lock().await;
        pending.insert(device_id.to_string(), pairing.clone());

        pairing
    }

    /// Validate a code and, if correct, issue a device_token and store the
    /// device as paired.
    pub async fn confirm(
        &self,
        device_id: &str,
        code: &str,
        display_name: &str,
        platform: &str,
    ) -> Result<PairedDevice, PairingError> {
        let now = chrono::Utc::now().timestamp_millis();

        // Validate the pending pairing
        let pending_entry = {
            let pending = self.pending.lock().await;
            pending
                .get(device_id)
                .cloned()
                .ok_or_else(|| PairingError::NoPendingPairing(device_id.to_string()))?
        };

        if now > pending_entry.expires_at {
            // Remove the expired entry
            let mut pending = self.pending.lock().await;
            pending.remove(device_id);
            return Err(PairingError::Expired);
        }

        if pending_entry.code != code {
            return Err(PairingError::InvalidCode);
        }

        // Generate 32-byte (64-char hex) random token
        let token_bytes: [u8; 32] = rand::rng().random();
        let device_token = token_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();

        let device = PairedDevice {
            device_id: device_id.to_string(),
            device_token,
            paired_at: now,
            display_name: display_name.to_string(),
            platform: platform.to_string(),
        };

        // Remove pending and store as paired
        {
            let mut pending = self.pending.lock().await;
            pending.remove(device_id);
        }
        {
            let mut paired = self.paired.write().await;
            paired.retain(|d| d.device_id != device_id); // replace any prior pairing
            paired.push(device.clone());
        }

        Ok(device)
    }

    /// Check whether a device has completed pairing.
    pub async fn is_paired(&self, device_id: &str) -> bool {
        let paired = self.paired.read().await;
        paired.iter().any(|d| d.device_id == device_id)
    }

    /// Verify a device's bearer token.
    pub async fn verify_token(&self, device_id: &str, token: &str) -> bool {
        let paired = self.paired.read().await;
        paired
            .iter()
            .any(|d| d.device_id == device_id && d.device_token == token)
    }

    /// Get all paired devices.
    pub async fn get_paired(&self) -> Vec<PairedDevice> {
        self.paired.read().await.clone()
    }

    /// Revoke pairing for a device.  Returns `true` if the device was found
    /// and removed.
    pub async fn revoke(&self, device_id: &str) -> bool {
        let mut paired = self.paired.write().await;
        let before = paired.len();
        paired.retain(|d| d.device_id != device_id);
        paired.len() < before
    }

    /// Serialize the paired devices list for persistence in settings.
    pub async fn to_records(&self) -> Vec<PairedDeviceRecord> {
        let paired = self.paired.read().await;
        paired
            .iter()
            .cloned()
            .map(PairedDeviceRecord::from)
            .collect()
    }
}

impl Default for PairingManager {
    fn default() -> Self {
        Self::new()
    }
}
