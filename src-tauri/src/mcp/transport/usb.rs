//! USB/ADB transport for physical Android device connections.
//!
//! Plan 1A refactor: device listing, shell commands, and `establish_forward`
//! now use the pure-Rust `adb_client` crate via `mcp::adb_helper`. The one
//! remaining subprocess call is `release_forward`, which still shells out to
//! `adb forward --remove tcp:<port>` because `adb_client` 3.x exposes only
//! `forward_remove_all` (nuclear) and not per-forward `killforward`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::TransportError;
use crate::mcp::adb_helper;

// ============================================================================
// UsbTransport
// ============================================================================

/// Manages ADB port forwards for connected Android devices.
///
/// `adb_path` is still used by `release_forward` because `adb_client` 3.x has
/// no per-forward `killforward` API (only `forward_remove_all`, which would
/// stomp every forward on the machine). Everything else (device listing,
/// shell, screenshot, logcat, `establish_forward`) goes through `adb_helper`.
///
/// `Clone` is cheap: `active_forwards` is an `Arc<Mutex<_>>` shared across
/// clones, and `adb_path` is a small `PathBuf`. We rely on cloning so the
/// shutdown handler in `main.rs` can call `release_all` on the same forward
/// registry the USB scanner task owns.
#[derive(Clone)]
pub struct UsbTransport {
    /// Path to the `adb` (or `adb.exe`) binary. Still needed for
    /// `release_forward` — see struct-level doc above.
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

    /// Establish an `adb forward tcp:<local> tcp:<remote>` using `adb_client`'s
    /// library-level `host:forward` call. Pre-allocates the local port on the
    /// loopback interface because the library discards the response body and
    /// cannot return an adb-chosen port. Stores the mapping for later teardown.
    pub async fn establish_forward(
        &self,
        adb_serial: &str,
        remote_port: u16,
    ) -> Result<u16, TransportError> {
        let local_port = adb_helper::pick_free_local_port()
            .await
            .map_err(|e| TransportError::ForwardFailed {
                device_id: adb_serial.to_string(),
                reason: format!("pick local port: {e}"),
            })?;

        adb_helper::forward_tcp(adb_serial.to_string(), local_port, remote_port)
            .await
            .map_err(|e| TransportError::ForwardFailed {
                device_id: adb_serial.to_string(),
                reason: e,
            })?;

        info!(
            serial = %adb_serial,
            local_port,
            remote_port,
            "ADB forward established"
        );

        self.active_forwards
            .lock()
            .await
            .insert(adb_serial.to_string(), local_port);

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

    /// Enumerate connected ADB devices via the pure-Rust `adb_client` crate.
    /// Returns `(serial, state, model_option)` tuples so callers remain compatible
    /// with the previous subprocess-based API.
    pub async fn scan_devices(&self) -> Vec<(String, String, Option<String>)> {
        adb_helper::list_devices()
            .await
            .into_iter()
            .map(|d| (d.serial, d.state, d.model))
            .collect()
    }

    /// Enable ADB TCP/IP mode on the device so it can be reached over Wi-Fi.
    /// After this succeeds, `{device_ip}:{port}` accepts `adb connect` requests.
    pub async fn enable_tcpip(&self, adb_serial: &str, port: u16) -> Result<(), TransportError> {
        adb_helper::enable_tcpip(adb_serial.to_string(), port)
            .await
            .map_err(|e| TransportError::AdbCommandFailed(e))
    }

    /// Read the device's Wi-Fi IPv4 address (via `ip route` / `ip addr`).
    pub async fn device_wlan_ip(&self, adb_serial: &str) -> Result<Option<String>, TransportError> {
        adb_helper::get_wlan_ipv4(adb_serial.to_string())
            .await
            .map_err(|e| TransportError::AdbCommandFailed(e))
    }
}
