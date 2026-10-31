//! Async wrappers around the `adb_client` crate.
//!
//! Plan 1A: replaces fragile subprocess `adb.exe` output parsing with a pure-Rust
//! library. Every operation runs on `tokio::task::spawn_blocking` because the
//! underlying `adb_client` API is synchronous.
//!
//! Port forwarding: `establish_forward` uses `adb_client`'s `forward()` API
//! (sends `host:forward:<local>;<remote>`). Because the library's `forward()`
//! discards the response body, callers must pre-allocate the local port via
//! `pick_free_local_port` rather than passing `tcp:0`. Per-forward and
//! per-reverse teardown use `forward_remove` / `reverse_remove` (added
//! upstream in adb_client 3.2.1 — cocool97/adb_client#192 and #195).

use std::collections::BTreeSet;
use std::io::Cursor;
use std::net::{Ipv4Addr, SocketAddrV4};

use adb_client::server::ADBServer;
use adb_client::server_device::ADBServerDevice;
use adb_client::ADBDeviceExt;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

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

/// Loopback ADB server address, on the port the environment names if it names one
/// here.
fn default_server_addr() -> SocketAddrV4 {
    let port = adb_server()
        .filter(|(host, _)| is_loopback_host(host))
        .map_or(5037, |(_, port)| port);
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// The `(host, port)` of the adb server this environment names, read the way `adb`
/// itself reads it (AOSP `client/commandline.cpp`). `None` if it names one this cannot
/// dial, such as a Unix socket, or a bad value.
fn adb_server() -> Option<(String, u16)> {
    let var = |name| std::env::var(name).ok();
    let socket = var("ADB_SERVER_SOCKET");
    let address = var("ANDROID_ADB_SERVER_ADDRESS");
    let port = var("ANDROID_ADB_SERVER_PORT");
    adb_server_from(socket.as_deref(), address.as_deref(), port.as_deref())
}

/// `ADB_SERVER_SOCKET` wins over the address and port variables. An empty port counts
/// as unset, and adb accepts no port 0.
fn adb_server_from(
    socket: Option<&str>,
    address: Option<&str>,
    port: Option<&str>,
) -> Option<(String, u16)> {
    let tcp_port = |port: &str| port.parse::<u16>().ok().filter(|port| *port != 0);
    if let Some(socket) = socket {
        let spec = socket.strip_prefix("tcp:")?;
        return match spec.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                let host = host.trim_start_matches('[').trim_end_matches(']');
                Some((host.to_string(), tcp_port(port)?))
            }
            Some(_) => None,
            None => Some(("127.0.0.1".to_string(), tcp_port(spec)?)),
        };
    }
    let host = match address {
        Some("") => return None,
        Some(host) => host,
        None => "127.0.0.1",
    };
    let port = match port.filter(|port| !port.is_empty()) {
        Some(port) => tcp_port(port)?,
        None => 5037,
    };
    Some((host.to_string(), port))
}

/// Best-effort classifier for an adb serial / transport id.
///
/// Used by the vision frame-source resolver to decide whether an opaque
/// `target` string should be routed to `adb` (device framebuffer) when it
/// isn't a registered physical device or app. Conservative on purpose — a
/// false positive routes a non-adb id to adb and surfaces a clean "device
/// not found" error, never a panic.
///
/// Matches the three shapes `adb devices` actually emits:
///   - emulator transport ids: `emulator-5554` (`emulator-<digits>`)
///   - networked devices: `host:port`, e.g. `192.168.1.20:5555` (contains `:`)
///   - USB hardware serials: non-empty run of `[A-Za-z0-9._-]` (e.g.
///     `R3CN30XXXXX`, `0123456789ABCDEF`). Rejects strings with spaces,
///     slashes, or other punctuation that an app-id / arbitrary label might
///     carry.
pub fn is_adb_serial(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // emulator-<digits>
    if let Some(rest) = s.strip_prefix("emulator-") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    // host:port (networked adb). Require a port-ish suffix after the last ':'.
    if let Some((host, port)) = s.rsplit_once(':') {
        let host_ok = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        let port_ok = !port.is_empty() && port.chars().all(|c| c.is_ascii_digit());
        return host_ok && port_ok;
    }
    // USB hardware serial: alphanumeric with the limited punctuation adb allows.
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

// ----------------------------------------------------------------------------
// Operations
// ----------------------------------------------------------------------------

/// List all devices visible to the local adb server.
///
/// Returns an empty vec on error so callers can treat "no devices" uniformly.
pub async fn list_devices() -> Vec<AdbDeviceInfo> {
    spawn_blocking_tracked(|| -> Vec<AdbDeviceInfo> {
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

/// Whether `serial` is currently listed by the local adb server as a device.
///
/// Used by the vision frame-source resolver to distinguish a real adb target
/// from an arbitrary label that merely *looks* serial-shaped (e.g. `bogus-id`).
/// A device in the `offline`/`unauthorized` state still counts as "connected"
/// here — it is a genuine adb target, just not ready; surfacing the adb-side
/// error is more useful than masking it as "unknown vision target".
///
/// Returns `false` when adb is unreachable or the serial is absent.
pub async fn is_connected_device(serial: &str) -> bool {
    let serial = serial.trim();
    if serial.is_empty() {
        return false;
    }
    list_devices().await.iter().any(|d| d.serial == serial)
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
    spawn_blocking_tracked(move || -> Result<Vec<u8>, String> {
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
    spawn_blocking_tracked(move || -> Result<Vec<u8>, String> {
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
    spawn_blocking_tracked(move || -> Result<Vec<u8>, String> {
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
    spawn_blocking_tracked(|| {
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
    let leaves_this_machine = forward_leaves_this_machine(&serial);
    with_forwarder_mark(leaves_this_machine, local_port, async move {
        spawn_blocking_tracked(move || -> Result<(), String> {
            let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
            device
                .forward(format!("tcp:{remote_port}"), format!("tcp:{local_port}"))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("task join failed: {e}"))?
    })
    .await
}

/// Whether a forward to `serial` delivers to a different machine: a phone on a
/// cable, or a device `adb connect`ed over the network. Only a local emulator
/// (`emulator-5554`) is on this machine. A USB serial is reported by the device,
/// so this trusts a phone not to name itself an emulator. A serial on a loopback
/// address (`adb connect 127.0.0.1:5555`) counts as leaving too, since an `ssh -L`
/// can sit behind one, so a local VM attached that way gets no bearer. A tunnel on a
/// port adb scans for emulators is not ruled out.
fn forward_leaves_this_machine(serial: &str) -> bool {
    !serial
        .strip_prefix("emulator-")
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

/// How long one read of the adb server's forward list may take. A healthy server
/// answers from memory, but a refusal is the ordinary answer on a box with no adb
/// server, and a refused connect has been measured at about 2s on Windows (to `[::1]`;
/// this read dials `127.0.0.1`, which has not been measured). It stays under the 5s a
/// step's reconnect gives the whole connect.
const FORWARD_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Read the adb server's forward list into the off-machine forwarder state, so a
/// forward this process did not open is recognised too: `adb forward` by hand,
/// another runner, or an earlier run of this one (the adb server outlives us).
///
/// Returns whether the list is known. No server listening means no forwards, which
/// is known. A server that cannot be read, or a list with a line that cannot be,
/// leaves the last list in place and answers `false`: any loopback port might then
/// be a forward to a phone, so no new handshake sends a bearer on that answer. A
/// stored client keeps its last decision until the list next reads, unless its port
/// is already known as a forwarder, and a connect or switch refuses it meanwhile.
///
/// A server on another host is read like any other, which catches a host that names
/// this machine. Its list then replaces this machine's server's: a forward on the
/// local server goes unseen, and a port it lists for anything but a local-emulator
/// serial is withheld from here too.
pub async fn refresh_adb_listed_forwarders() -> bool {
    let read = crate::mcp::transport::next_adb_list_read();
    let Some((host, port)) = adb_server() else {
        warn!("the environment names an adb server whose forward list cannot be read");
        return false;
    };
    let answer = tokio::time::timeout(FORWARD_LIST_TIMEOUT, read_forward_list(&host, port)).await;
    match answer {
        Ok(Ok(list)) => match ports_forwarding_off_this_machine(&list) {
            Some(ports) => {
                crate::mcp::transport::set_adb_listed_forwarders(read, ports);
                true
            }
            None => {
                warn!("could not parse the adb forward list: {list:?}");
                false
            }
        },
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            crate::mcp::transport::set_adb_listed_forwarders(read, BTreeSet::new());
            true
        }
        Ok(Err(e)) => {
            warn!("could not read the adb forward list: {e}");
            false
        }
        Err(_) => {
            warn!("timed out reading the adb forward list");
            false
        }
    }
}

/// `host:list-forward` on the adb wire, which `adb_client` does not expose: a
/// hex-length-prefixed request, then `OKAY` and a hex-length-prefixed body of
/// `<serial> <local> <remote>` lines.
async fn read_forward_list(host: &str, port: u16) -> std::io::Result<String> {
    const REQUEST: &str = "host:list-forward";
    let bad_reply = |reply: String| std::io::Error::new(std::io::ErrorKind::InvalidData, reply);

    let mut stream = tokio::net::TcpStream::connect((host, port)).await?;
    stream
        .write_all(format!("{:04x}{REQUEST}", REQUEST.len()).as_bytes())
        .await?;

    let mut status = [0u8; 4];
    stream.read_exact(&mut status).await?;
    if &status != b"OKAY" {
        return Err(bad_reply(format!(
            "status {}",
            String::from_utf8_lossy(&status)
        )));
    }
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).await?;
    let length = std::str::from_utf8(&length)
        .ok()
        .and_then(|length| usize::from_str_radix(length, 16).ok())
        .ok_or_else(|| bad_reply(format!("length {length:?}")))?;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The local TCP ports in a `host:list-forward` body whose device is not on this
/// machine, or `None` if a line cannot be read, since it might name any port.
fn ports_forwarding_off_this_machine(list: &str) -> Option<BTreeSet<u16>> {
    let mut ports = BTreeSet::new();
    for line in list.lines().filter(|line| !line.is_empty()) {
        let (serial, local) = split_forward_line(line)?;
        if let Some(port) = local.strip_prefix("tcp:") {
            let port: u16 = port.parse().ok()?;
            if forward_leaves_this_machine(&serial) {
                ports.insert(port);
            }
        }
    }
    Some(ports)
}

/// The `(serial, local)` of a `<serial> <local> <remote>` line. The serial is the
/// device's own string and the remote the forward's author's, and either can hold a
/// space, so the local end is found by its shape: the first field after the first
/// that names a socket the way `adb forward` spells one. A line naming a third socket
/// says nothing certain about which is the local end, so it is not read.
fn split_forward_line(line: &str) -> Option<(String, &str)> {
    const SOCKET_SPECS: [&str; 10] = [
        "tcp:",
        "local:",
        "localabstract:",
        "localreserved:",
        "localfilesystem:",
        "dev:",
        "dev-raw:",
        "jdwp:",
        "vsock:",
        "acceptfd:",
    ];
    let fields: Vec<&str> = line.split(' ').collect();
    let mut sockets = fields
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, field)| SOCKET_SPECS.iter().any(|spec| field.starts_with(spec)));
    let (local_at, local) = sockets.next()?;
    if sockets.nth(1).is_some() {
        return None;
    }
    Some((fields.get(..local_at)?.join(" "), *local))
}

/// Run `forward`, the adb call that opens `local_port`, with the port already
/// marked as an off-machine forwarder when it `leaves_this_machine`, so no
/// connect can dial it unmarked. A failed forward opened nothing, so its mark
/// is dropped.
async fn with_forwarder_mark<F>(
    leaves_this_machine: bool,
    local_port: u16,
    forward: F,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
{
    if leaves_this_machine {
        crate::mcp::transport::mark_off_machine_forwarder(local_port);
    }
    let forwarded = forward.await;
    if leaves_this_machine && forwarded.is_err() {
        crate::mcp::transport::unmark_off_machine_forwarder(local_port);
    }
    forwarded
}

/// Remove a single adb reverse by its device-side ("remote") endpoint
/// (`reverse:killforward:tcp:<device_port>`).
///
/// Available since `adb_client` 3.2.1 (cocool97/adb_client#195).
pub async fn reverse_remove(serial: String, device_port: u16) -> Result<(), String> {
    spawn_blocking_tracked(move || -> Result<(), String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        device
            .reverse_remove(format!("tcp:{device_port}"))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))?
}

/// Establish an `adb reverse tcp:<device_port> tcp:<host_port>` on the device.
///
/// This is the mirror of [`forward_tcp`]: it makes the *device* listen on
/// `device_port` and tunnel those connections back to `host_port` on the
/// machine running the runner. The qontinui-mobile app's data path dials the
/// runner HTTP API (default 9876) at `localhost:<device_port>` on the phone;
/// without this reverse the request fails with "Network request failed"
/// because nothing on the USB-attached phone serves that port.
///
/// Argument order matches `adb_client`'s `reverse(remote, local)` and the ADB
/// wire format `reverse:forward:<remote>;<local>`, where `remote` is the
/// device-side listen port and `local` is the host-side target port.
///
/// Idempotent: re-running an identical reverse simply overwrites the existing
/// rule in adb (it does not error), so this is safe to call on every scan tick.
pub async fn reverse_tcp(serial: String, device_port: u16, host_port: u16) -> Result<(), String> {
    spawn_blocking_tracked(move || -> Result<(), String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        device
            .reverse(format!("tcp:{device_port}"), format!("tcp:{host_port}"))
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
    spawn_blocking_tracked(|| -> Result<u16, String> {
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

/// Remove a single adb forward by its local endpoint
/// (`host-serial:<serial>:killforward:tcp:<local_port>`).
///
/// Available since `adb_client` 3.2.1 (cocool97/adb_client#192).
pub async fn forward_remove(serial: String, local_port: u16) -> Result<(), String> {
    let leaves_this_machine = forward_leaves_this_machine(&serial);
    spawn_blocking_tracked(move || -> Result<(), String> {
        let mut device = ADBServerDevice::new(serial, Some(default_server_addr()));
        device
            .forward_remove(format!("tcp:{local_port}"))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join failed: {e}"))??;
    if leaves_this_machine {
        crate::mcp::transport::unmark_off_machine_forwarder(local_port);
    }
    Ok(())
}

/// Remove **all** adb forwards (`host:killforward-all`).
///
/// Nuclear option — wipes every forward on the local adb server, not just this
/// process's. Unused by current callers; prefer [`forward_remove`] for cleanup.
#[allow(dead_code)]
pub async fn forward_remove_all() -> Result<(), String> {
    spawn_blocking_tracked(|| -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::is_adb_serial;

    #[test]
    fn only_a_local_emulator_forward_stays_on_this_machine() {
        assert!(!super::forward_leaves_this_machine("emulator-5554"));
        assert!(super::forward_leaves_this_machine("emulator-abc"));
        assert!(super::forward_leaves_this_machine("emulator-"));
        for serial in [
            "R3CN30ABCDEF",
            "127.0.0.1:5555",
            "192.168.1.5:5555",
            "[fe80::1]:5555",
            "adb-R3CN30ABCDE-a1b2c3._adb-tls-connect._tcp",
        ] {
            assert!(super::forward_leaves_this_machine(serial), "{serial}");
        }
    }

    /// The port is marked before adb opens it, and a failed forward, which
    /// opened nothing, drops the mark. The port is held bound throughout so no
    /// peer test can be handed it.
    #[tokio::test]
    async fn a_forward_port_is_marked_before_adb_opens_it_and_only_while_it_exists() {
        use crate::mcp::transport::{is_off_machine_forwarder, ForgetForwarder};

        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        let _forget = ForgetForwarder(port);

        super::with_forwarder_mark(true, port, async {
            assert!(is_off_machine_forwarder(port), "marked before the forward");
            Err("adb refused".to_string())
        })
        .await
        .expect_err("the forward failed");
        assert!(
            !is_off_machine_forwarder(port),
            "a failed forward stays unmarked"
        );

        super::with_forwarder_mark(true, port, async { Ok(()) })
            .await
            .expect("the forward succeeded");
        assert!(
            is_off_machine_forwarder(port),
            "a live forward stays marked"
        );
    }

    /// A forward list names a port only for a device off this machine, and skips
    /// a local end that is not a TCP port.
    #[test]
    fn the_forward_list_yields_the_ports_that_leave_this_machine() {
        let list = "R3CN30ABCDEF tcp:6100 tcp:9876\n\
                    emulator-5554 tcp:6101 tcp:9876\n\
                    192.168.1.5:5555 tcp:6102 tcp:9876\n\
                    R3CN30ABCDEF localabstract:foo tcp:9876\n\
                    serial with spaces tcp:6103 tcp:9876\n";
        assert_eq!(
            super::ports_forwarding_off_this_machine(list),
            Some(std::collections::BTreeSet::from([6100, 6102, 6103]))
        );
    }

    /// A remote holding a space does not move the local end, and a line with no
    /// readable local end makes the whole list unknown rather than one port shorter.
    #[test]
    fn a_forward_line_is_read_by_its_local_end_or_not_at_all() {
        assert_eq!(
            super::ports_forwarding_off_this_machine("R3CN30ABCDEF tcp:6100 localabstract:a b\n"),
            Some(std::collections::BTreeSet::from([6100]))
        );
        for unreadable in [
            "R3CN30ABCDEF\n",
            "R3CN30ABCDEF 6100 9876\n",
            "R3CN30ABCDEF tcp:notaport tcp:9876\n",
            "named tcp:1 tcp:6100 tcp:9876\n",
            "R3CN30ABCDEF tcp:6100 tcp:9876\ngarbage\n",
        ] {
            assert_eq!(
                super::ports_forwarding_off_this_machine(unreadable),
                None,
                "{unreadable}"
            );
        }
    }

    /// The environment names the adb server the way `adb` reads it, and one that
    /// cannot be dialled is no server at all.
    #[test]
    fn the_adb_server_is_read_the_way_adb_reads_it() {
        let server = super::adb_server_from;
        let on = |host: &str, port| Some((host.to_string(), port));
        assert_eq!(server(None, None, None), on("127.0.0.1", 5037));
        assert_eq!(server(None, None, Some("")), on("127.0.0.1", 5037));
        assert_eq!(server(None, None, Some("5038")), on("127.0.0.1", 5038));
        assert_eq!(
            server(None, Some("10.0.0.2"), Some("5038")),
            on("10.0.0.2", 5038)
        );
        assert_eq!(
            server(Some("tcp:5039"), Some("10.0.0.2"), Some("5038")),
            on("127.0.0.1", 5039)
        );
        assert_eq!(
            server(Some("tcp:localhost:5039"), None, None),
            on("localhost", 5039)
        );
        assert_eq!(server(Some("tcp:[::1]:5039"), None, None), on("::1", 5039));
        for undialable in [
            server(None, None, Some("0")),
            server(None, None, Some("notaport")),
            server(None, Some(""), None),
            server(Some("tcp::5039"), None, None),
            server(Some("tcp:notaport"), None, None),
            server(Some("localfilesystem:/tmp/adb"), None, None),
            server(Some(""), None, None),
        ] {
            assert_eq!(undialable, None);
        }
    }

    /// A failed emulator forward made no mark, so it drops none, even where a
    /// peer forwarder holds one on that port.
    #[tokio::test]
    async fn a_failed_forward_on_this_machine_drops_no_mark() {
        use crate::mcp::transport::{
            is_off_machine_forwarder, mark_off_machine_forwarder, ForgetForwarder,
        };
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        mark_off_machine_forwarder(port);
        let _forget = ForgetForwarder(port);
        super::with_forwarder_mark(false, port, async { Err("adb refused".to_string()) })
            .await
            .expect_err("the forward failed");
        assert!(is_off_machine_forwarder(port));
    }

    /// A local emulator's forward is never marked.
    #[tokio::test]
    async fn a_forward_on_this_machine_is_not_marked() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        super::with_forwarder_mark(false, port, async { Ok(()) })
            .await
            .expect("the forward succeeded");
        assert!(!crate::mcp::transport::is_off_machine_forwarder(port));
    }

    #[test]
    fn classifies_adb_serials() {
        // emulator transport ids
        assert!(is_adb_serial("emulator-5554"));
        assert!(is_adb_serial("emulator-0"));
        // networked devices (host:port)
        assert!(is_adb_serial("192.168.1.20:5555"));
        assert!(is_adb_serial("phone.local:5555"));
        // USB hardware serials
        assert!(is_adb_serial("R3CN30ABCDEF"));
        assert!(is_adb_serial("0123456789ABCDEF"));
        assert!(is_adb_serial("HT4-A1B2"));

        // Non-serials
        assert!(!is_adb_serial(""));
        assert!(!is_adb_serial("   "));
        assert!(!is_adb_serial("emulator-"));
        assert!(!is_adb_serial("emulator-abc"));
        assert!(!is_adb_serial("192.168.1.20:")); // empty port
        assert!(!is_adb_serial(":5555")); // empty host
        assert!(!is_adb_serial("host:notaport"));
        assert!(!is_adb_serial("my app id")); // space
        assert!(!is_adb_serial("https://example.com/app")); // slash + scheme
    }
}
