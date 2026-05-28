//! USB/ADB transport for physical Android device connections.
//!
//! Plan 1A refactor: every ADB operation this transport performs (device
//! listing, shell, screenshot, logcat, forward/reverse establish, forward/
//! reverse per-rule removal) goes through the pure-Rust `adb_client` crate
//! via `mcp::adb_helper`. No subprocess fallback remains.

use std::collections::HashMap;
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
/// All ADB I/O goes through `adb_helper` (the pure-Rust `adb_client` crate);
/// no subprocess fallback is needed.
///
/// `Clone` is cheap — both registries are `Arc<Mutex<_>>` shared across clones.
/// We rely on cloning so the shutdown handler in `main.rs` can call
/// `release_all` on the same forward/reverse registries the USB scanner task
/// owns.
#[derive(Clone)]
pub struct UsbTransport {
    /// Maps ADB serial number → locally forwarded TCP port.
    pub active_forwards: Arc<Mutex<HashMap<String, u16>>>,
    /// Maps ADB serial number → device-side port currently reversed back to the
    /// runner HTTP API (`adb reverse tcp:<port> tcp:<port>`). Tracked so the
    /// shutdown / disconnect paths can remove exactly the rules this process
    /// installed, rather than nuking every reverse on the machine.
    pub active_reverses: Arc<Mutex<HashMap<String, u16>>>,
}

impl UsbTransport {
    pub fn new() -> Self {
        Self {
            active_forwards: Arc::new(Mutex::new(HashMap::new())),
            active_reverses: Arc::new(Mutex::new(HashMap::new())),
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
        let local_port = adb_helper::pick_free_local_port().await.map_err(|e| {
            TransportError::ForwardFailed {
                device_id: adb_serial.to_string(),
                reason: format!("pick local port: {e}"),
            }
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

        if let Err(e) = adb_helper::forward_remove(adb_serial.to_string(), port).await {
            warn!(
                serial = %adb_serial,
                port,
                error = %e,
                "Failed to remove ADB forward (device may be disconnected)"
            );
        }

        let mut forwards = self.active_forwards.lock().await;
        forwards.remove(adb_serial);

        debug!(serial = %adb_serial, port, "ADB forward released");
        Ok(())
    }

    /// Establish an `adb reverse tcp:<runner_port> tcp:<runner_port>` so the
    /// USB-attached device can reach the runner's HTTP API (default 9876) at
    /// `localhost:<runner_port>` on the phone.
    ///
    /// This is the data-path counterpart to [`establish_forward`] (which is the
    /// control-path: host → device UI Bridge). Without it, qontinui-mobile's
    /// requests to the runner API fail with "Network request failed" because
    /// nothing on the phone serves that port. Same port on both ends — the
    /// device dials the identical port number it would use on the host.
    ///
    /// Idempotent: adb overwrites an identical reverse rule without erroring, so
    /// this is safe to re-run on every scan tick. Records the rule for teardown.
    ///
    /// [`establish_forward`]: UsbTransport::establish_forward
    pub async fn establish_reverse(
        &self,
        adb_serial: &str,
        runner_port: u16,
    ) -> Result<(), TransportError> {
        adb_helper::reverse_tcp(adb_serial.to_string(), runner_port, runner_port)
            .await
            .map_err(|e| TransportError::ForwardFailed {
                device_id: adb_serial.to_string(),
                reason: e,
            })?;

        info!(
            serial = %adb_serial,
            runner_port,
            "ADB reverse established (device -> runner API)"
        );

        self.active_reverses
            .lock()
            .await
            .insert(adb_serial.to_string(), runner_port);

        Ok(())
    }

    /// Remove the ADB reverse for the given device and clean up the map entry.
    pub async fn release_reverse(&self, adb_serial: &str) -> Result<(), TransportError> {
        let port = {
            let reverses = self.active_reverses.lock().await;
            match reverses.get(adb_serial).copied() {
                Some(p) => p,
                None => {
                    debug!(serial = %adb_serial, "release_reverse: no active reverse found");
                    return Ok(());
                }
            }
        };

        if let Err(e) = adb_helper::reverse_remove(adb_serial.to_string(), port).await {
            warn!(
                serial = %adb_serial,
                port,
                error = %e,
                "Failed to remove ADB reverse (device may be disconnected)"
            );
        }

        let mut reverses = self.active_reverses.lock().await;
        reverses.remove(adb_serial);

        debug!(serial = %adb_serial, port, "ADB reverse released");
        Ok(())
    }

    /// Release all active forwards and reverses. Errors are logged but do not
    /// stop teardown.
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

        let reverse_serials: Vec<String> = {
            let reverses = self.active_reverses.lock().await;
            reverses.keys().cloned().collect()
        };
        for serial in reverse_serials {
            if let Err(e) = self.release_reverse(&serial).await {
                warn!(serial = %serial, error = %e, "Error releasing ADB reverse");
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
