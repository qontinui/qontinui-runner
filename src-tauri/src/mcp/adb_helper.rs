//! Async wrappers around the `adb_client` crate.
//!
//! Plan 1A: replaces fragile subprocess `adb.exe` output parsing with a pure-Rust
//! library. Every operation runs on `tokio::task::spawn_blocking` because the
//! underlying `adb_client` API is synchronous.
//!
//! Port forwarding: `establish_forward` now uses `adb_client`'s `forward()` API
//! (sends `host:forward:<local>;<remote>`). Because the library's `forward()`
//! discards the response body, callers must pre-allocate the local port via
//! `pick_free_local_port` rather than passing `tcp:0`. Per-forward removal
//! (`host:killforward:tcp:<port>`) is not exposed, so teardown callers still
//! shell out; only `forward_remove_all` (kills every forward) is available here.

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

/// Establish an `adb forward tcp:<local> tcp:<remote>` on the given device.
///
/// Caller must pre-allocate `local_port` via `pick_free_local_port`, because
/// `adb_client`'s `forward()` drops the response body and cannot return the
/// port the server picks when you pass `tcp:0`.
///
/// Note on argument order: `adb_client::ADBServerDevice::forward` takes
/// `(remote, local)` (the ADB wire format is `host:forward:<local>;<remote>`
/// but the `Forward(remote, local)` enum variant reorders them for the caller).
pub async fn forward_tcp(serial: String, local_port: u16, remote_port: u16) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        device
            .forward(format!("tcp:{remote_port}"), format!("tcp:{local_port}"))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Pick a free loopback TCP port by briefly binding to `127.0.0.1:0`.
///
/// The OS returns a port that's free at the moment of binding; there is a
/// small TOCTOU window between the drop and the subsequent `adb forward`
/// call, but in practice this is how every tool in the ecosystem does it.
pub async fn pick_free_local_port() -> Result<u16, String> {
    tokio::task::spawn_blocking(|| -> Result<u16, String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("local_addr: {e}"))?
            .port();
        // listener is dropped here, releasing the port for adb to claim.
        Ok(port)
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Remove **all** adb forwards (`host:killforward-all`).
///
/// Currently unused by callers but exposed for future teardown paths. Because
/// there is no per-forward `killforward` API in `adb_client` 3.x, this is the
/// only library-level removal available; individual forward cleanup still
/// shells out to `adb forward --remove tcp:<port>`.
#[allow(dead_code)]
pub async fn forward_remove_all() -> Result<(), String> {
    tokio::task::spawn_blocking(|| -> Result<(), String> {
        // `forward_remove_all` is defined on ADBServerDevice, but the wire
        // command it sends (`host:killforward-all`) targets the server, not a
        // specific device. Using an empty serial is fine because the server
        // ignores the device selector for this command.
        let mut device = ADBServerDevice::new(String::new(), Some(default_server_addr()));
        device.forward_remove_all().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

// Keep an unused import suppressed explicitly; Cursor is used by downstream
// callers that may want to wrap byte buffers.
#[allow(dead_code)]
fn _keep_cursor_in_scope() -> Cursor<Vec<u8>> {
    Cursor::new(Vec::new())
}
