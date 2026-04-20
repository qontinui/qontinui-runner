//! Async wrappers around the `adb_client` crate.
//!
//! Plan 1A: replaces fragile subprocess `adb.exe` output parsing with a pure-Rust
//! library. Every operation runs on `tokio::task::spawn_blocking` because the
//! underlying `adb_client` API is synchronous.
//!
//! Port forwarding (`adb forward`) is NOT exposed by `adb_client` 3.x, so callers
//! that need it still shell out via `crate::process_helpers::tokio_no_window`.
//! Everything else (listing, shell, pull, screenshot, tcpip) routes through here.

use std::io::Cursor;
use std::net::SocketAddrV4;

use adb_client::server::ADBServer;
use adb_client::server_device::ADBServerDevice;
use adb_client::ADBDeviceExt;

// ----------------------------------------------------------------------------
// Types
// ----------------------------------------------------------------------------

/// Mirror of `adb devices -l` entries.
#[derive(Debug, Clone)]
pub struct AdbDeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

/// Default loopback ADB server address (`127.0.0.1:5037`).
fn default_server_addr() -> SocketAddrV4 {
    "127.0.0.1:5037".parse().expect("static addr parses")
}

// ----------------------------------------------------------------------------
// Operations
// ----------------------------------------------------------------------------

/// List all devices visible to the local adb server.
///
/// Returns an empty vec on error so callers can treat "no devices" uniformly.
pub async fn list_devices() -> Vec<AdbDeviceInfo> {
    tokio::task::spawn_blocking(|| -> Vec<AdbDeviceInfo> {
        let mut server = ADBServer::new(default_server_addr());
        let devices = match server.devices_long() {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(error = %e, "adb_client: devices_long failed");
                return Vec::new();
            }
        };

        devices
            .into_iter()
            .map(|d| {
                // DeviceLong::Display renders fields; extract via the struct directly
                // by formatting and parsing. adb_client's DeviceLong exposes fields
                // but they vary across versions, so fall back to Debug when needed.
                let rendered = format!("{d}");
                parse_device_line(&rendered)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn parse_device_line(line: &str) -> AdbDeviceInfo {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let serial = parts.first().map(|s| (*s).to_string()).unwrap_or_default();
    let state = parts.get(1).map(|s| (*s).to_string()).unwrap_or_default();
    let model = parts
        .iter()
        .find(|p| p.starts_with("model:"))
        .map(|p| p.trim_start_matches("model:").to_string());
    AdbDeviceInfo {
        serial,
        state,
        model,
    }
}

/// Run a shell command on a device and return stdout as bytes.
pub async fn shell_capture(serial: String, command: String) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        let mut out: Vec<u8> = Vec::new();
        device
            .shell_command(&command, Some(&mut out), None)
            .map_err(|e| e.to_string())?;
        Ok(out)
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Convenience: run a shell command and return stdout as UTF-8 (lossy).
pub async fn shell_capture_string(serial: String, command: String) -> Result<String, String> {
    let bytes = shell_capture(serial, command).await?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Pull a remote file into memory.
pub async fn pull_bytes(serial: String, remote_path: String) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        let mut out: Vec<u8> = Vec::new();
        device
            .pull(&remote_path, &mut out)
            .map_err(|e| e.to_string())?;
        Ok(out)
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Capture a device screenshot as PNG bytes, skipping the `screencap -p`/`pull`/`rm`
/// dance. Uses `ADBDeviceExt::framebuffer_bytes`.
pub async fn screenshot_png(serial: String) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        device.framebuffer_bytes().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Enable ADB TCP/IP mode on the device so it can be reached over Wi-Fi at
/// `{device_ip}:{port}`. The device's adbd restarts after this.
pub async fn enable_tcpip(serial: String, port: u16) -> Result<(), String> {
    // `setprop service.adb.tcp.port <port>` + restart adbd.
    shell_capture(
        serial.clone(),
        format!("setprop service.adb.tcp.port {port}"),
    )
    .await?;
    let _ = shell_capture(serial.clone(), "stop adbd".to_string()).await;
    shell_capture(serial, "start adbd".to_string()).await?;
    Ok(())
}

/// Read the device's primary Wi-Fi IPv4 address. Tries `ip route` first (parses
/// `src <ip>`), then falls back to `ip -4 addr show wlan0`.
pub async fn get_wlan_ipv4(serial: String) -> Result<Option<String>, String> {
    let route = shell_capture_string(serial.clone(), "ip route".to_string()).await?;
    for line in route.lines() {
        let line = line.trim();
        if !line.contains("wlan") {
            continue;
        }
        if let Some(idx) = line.find("src ") {
            let rest = &line[idx + 4..];
            if let Some(ip) = rest.split_whitespace().next() {
                return Ok(Some(ip.to_string()));
            }
        }
    }

    let addr = shell_capture_string(serial, "ip -4 addr show wlan0".to_string()).await?;
    for line in addr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("inet ") {
            if let Some(cidr) = rest.split_whitespace().next() {
                if let Some((ip, _)) = cidr.split_once('/') {
                    return Ok(Some(ip.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// Sanity-check that the adb server is reachable. Used by discovery to decide
/// whether to attempt a `list_devices()` call at all.
pub async fn server_reachable() -> bool {
    tokio::task::spawn_blocking(|| {
        let mut server = ADBServer::new(default_server_addr());
        server.version().is_ok()
    })
    .await
    .unwrap_or(false)
}

// Keep an unused import suppressed explicitly; Cursor is used by downstream
// callers that may want to wrap byte buffers.
#[allow(dead_code)]
fn _keep_cursor_in_scope() -> Cursor<Vec<u8>> {
    Cursor::new(Vec::new())
}
