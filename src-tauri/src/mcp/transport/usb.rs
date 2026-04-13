//! USB/ADB transport for physical Android device connections.
//!
//! Establishes ADB port forwarding so a device's UI Bridge HTTP server
//! appears on a local 127.0.0.1 port, matching the pattern used by LAN
//! and Cloud transports.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::TransportError;

// ============================================================================
// UsbTransport
// ============================================================================

/// Manages ADB port forwards for connected Android devices.
pub struct UsbTransport {
    /// Path to the `adb` (or `adb.exe`) binary.
    pub adb_path: PathBuf,
    /// Maps ADB serial number → locally forwarded TCP port.
    pub active_forwards: Arc<Mutex<HashMap<String, u16>>>,
}

impl UsbTransport {
    pub fn new(adb_path: PathBuf) -> Self {
        Self {
            adb_path,
            active_forwards: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run `adb -s {serial} forward tcp:0 tcp:{remote_port}` and return the
    /// allocated local port.  Stores the mapping for later teardown.
    pub async fn establish_forward(
        &self,
        adb_serial: &str,
        remote_port: u16,
    ) -> Result<u16, TransportError> {
        let output = crate::process_helpers::tokio_no_window(&self.adb_path)
            .args([
                "-s",
                adb_serial,
                "forward",
                "tcp:0",
                &format!("tcp:{}", remote_port),
            ])
            .output()
            .await
            .map_err(|e| TransportError::AdbCommandFailed(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(TransportError::ForwardFailed {
                device_id: adb_serial.to_string(),
                reason: stderr,
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let local_port: u16 = stdout.parse().map_err(|_| TransportError::ForwardFailed {
            device_id: adb_serial.to_string(),
            reason: format!("could not parse local port from adb output: {:?}", stdout),
        })?;

        info!(
            serial = %adb_serial,
            local_port,
            remote_port,
            "ADB forward established"
        );

        let mut forwards = self.active_forwards.lock().await;
        forwards.insert(adb_serial.to_string(), local_port);

        Ok(local_port)
    }

    /// Remove the ADB forward for the given device and clean up the map entry.
    pub async fn release_forward(&self, adb_serial: &str) -> Result<(), TransportError> {
        let port = {
            let forwards = self.active_forwards.lock().await;
            match forwards.get(adb_serial).copied() {
                Some(p) => p,
                None => {
                    debug!(serial = %adb_serial, "release_forward: no active forward found");
                    return Ok(());
                }
            }
        };

        let output = crate::process_helpers::tokio_no_window(&self.adb_path)
            .args([
                "-s",
                adb_serial,
                "forward",
                "--remove",
                &format!("tcp:{}", port),
            ])
            .output()
            .await
            .map_err(|e| TransportError::AdbCommandFailed(e.to_string()))?;

        if !output.status.success() {
            warn!(
                serial = %adb_serial,
                port,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "Failed to remove ADB forward (device may be disconnected)"
            );
        }

        let mut forwards = self.active_forwards.lock().await;
        forwards.remove(adb_serial);

        debug!(serial = %adb_serial, port, "ADB forward released");
        Ok(())
    }

    /// Release all active forwards.  Errors are logged but do not stop teardown.
    pub async fn release_all(&self) {
        let serials: Vec<String> = {
            let forwards = self.active_forwards.lock().await;
            forwards.keys().cloned().collect()
        };
        for serial in serials {
            if let Err(e) = self.release_forward(&serial).await {
                warn!(serial = %serial, error = %e, "Error releasing ADB forward");
            }
        }
    }

    /// Return the locally forwarded port for a device, if one exists.
    pub async fn get_forwarded_port(&self, adb_serial: &str) -> Option<u16> {
        let forwards = self.active_forwards.lock().await;
        forwards.get(adb_serial).copied()
    }

    /// Run `adb devices -l` and parse the output.
    ///
    /// Returns a list of `(device_id, status, model_option)` tuples.
    /// This mirrors the parsing logic used in `app_discovery::list_adb_devices`.
    pub async fn scan_devices(&self) -> Vec<(String, String, Option<String>)> {
        let output = match crate::process_helpers::tokio_no_window(&self.adb_path)
            .args(["devices", "-l"])
            .output()
            .await
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            Ok(o) => {
                warn!(
                    stderr = %String::from_utf8_lossy(&o.stderr),
                    "adb devices -l returned non-zero status"
                );
                return Vec::new();
            }
            Err(e) => {
                warn!(error = %e, "Failed to run adb devices");
                return Vec::new();
            }
        };

        let mut devices = Vec::new();
        for line in output.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }

            let device_id = parts[0].to_string();
            let status = parts[1].to_string();
            let model = parts
                .iter()
                .find(|p| p.starts_with("model:"))
                .map(|p| p.trim_start_matches("model:").to_string());

            devices.push((device_id, status, model));
        }

        devices
    }
}
