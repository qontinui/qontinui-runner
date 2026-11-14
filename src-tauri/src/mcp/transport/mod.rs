//! Transport layer for physical device UI Bridge connections.
//!
//! Supports three transport kinds:
//! - USB — ADB port forwarding (highest priority, lowest latency)
//! - LAN — Direct Wi-Fi connection via TCP proxy
//! - Cloud — Relayed via qontinui.io backend WebSocket (fallback)

pub mod ios; // Plan 2: iOS device transport (pymobiledevice3 sidecar)
pub mod lan;
pub mod pairing;
pub mod usb;
// Cloud transport is a future phase — module declared but empty for now
pub mod cloud;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

// ============================================================================
// Off-machine forwarders
// ============================================================================

/// Local ports known to carry traffic to ANOTHER machine: a LAN device's TCP
/// proxy, a Cloud tunnel, an iOS sidecar forward, and an adb forward to anything
/// but a local emulator.
///
/// Such a port reads as loopback, so the URL alone cannot tell it from a relay
/// on this machine, while everything sent to it (the operator's bearer
/// included) reaches the far end. The SDK connect's bearer decision reads this
/// state: see `sdk_client::classify_bearer_target`.
///
/// It lives here rather than in `PhysicalDeviceRegistry` because a listener can
/// outlive its registry entry: the health monitor evicts a transport while a
/// detached LAN proxy task keeps accepting. A mark is dropped only by whoever
/// made it, when its forwarder is known to be gone (a removed adb forward, a
/// closed tunnel), so a LAN proxy or iOS forward stays marked for the life of the
/// process, as does an adb forward whose device was unplugged before it could be
/// removed. If the OS later reuses such a port for a real local relay, that relay
/// is sent no bearer, which fails closed.
///
/// `marked` knows only what this process opened. `adb_listed` is the adb server's
/// own list, which also holds forwards from `adb forward` by hand, another
/// runner, or an earlier run of this one. It is read at each connect and switch to a
/// loopback URL and every 30s, so a forward made by hand over a stored connection's
/// port goes unseen until the next of those. A forward made any other way (`ssh -L`,
/// another runner's LAN proxy) reads as an ordinary loopback relay, and so does a
/// forward on an adb server this process was not told of: nothing listening where
/// `adb_helper` looks reads as no forwards, and only the one server it is told of
/// is read.
///
/// A LAN proxy is marked whatever its address, so one a caller points at loopback
/// gets no bearer, as an `adb connect` to loopback gets none.
struct OffMachineForwarders {
    /// How many live forwarders have marked each port. A port can be handed to a
    /// second one in the moment after the first lets it go.
    marked: BTreeMap<u16, u32>,
    /// The subset of `marked` the iOS sidecar forwarded to a phone, counted the
    /// same way. The sidecar holds each such port while it runs, so nothing else can
    /// bind it meanwhile. An adb forward is not in here: adb drops a forward when its
    /// phone is unplugged and nothing here hears of it, so an adb forward is a cable
    /// only while the adb server lists it (`adb_cables`). A LAN proxy or relay tunnel
    /// is marked but is never a cable. See [`is_cable_forwarder`].
    cable: BTreeMap<u16, u32>,
    adb_listed: BTreeSet<u16>,
    /// The subset of `adb_listed` whose device is on a USB cable of an adb server on
    /// this machine. A device `adb connect`ed over the network is listed but is not
    /// in here.
    adb_cables: BTreeSet<u16>,
    /// The [`next_adb_list_read`] that `adb_listed` came from.
    adb_list_read: u64,
}

static OFF_MACHINE_FORWARDERS: Mutex<OffMachineForwarders> = Mutex::new(OffMachineForwarders {
    marked: BTreeMap::new(),
    cable: BTreeMap::new(),
    adb_listed: BTreeSet::new(),
    adb_cables: BTreeSet::new(),
    adb_list_read: 0,
});

fn off_machine_forwarders() -> MutexGuard<'static, OffMachineForwarders> {
    OFF_MACHINE_FORWARDERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Record `port` as an off-machine forwarder. Call it before the port is handed
/// to anyone, so no connect can dial it unmarked.
pub fn mark_off_machine_forwarder(port: u16) {
    *off_machine_forwarders().marked.entry(port).or_default() += 1;
}

/// Drop one [`mark_off_machine_forwarder`] once its forwarder is known to be
/// gone. Call it only for a mark the caller's own forwarder made.
pub fn unmark_off_machine_forwarder(port: u16) {
    let mut forwarders = off_machine_forwarders();
    if let Some(count) = forwarders.marked.get_mut(&port) {
        *count -= 1;
        if *count == 0 {
            forwarders.marked.remove(&port);
        }
    }
}

/// Record `port` as a forward the iOS sidecar opened to a phone. It is marked as an
/// off-machine forwarder too. Drop the cable half with [`drop_cable_mark`] when the
/// sidecar stops, since its ports are then free for anything to bind.
pub fn mark_cable_forwarder(port: u16) {
    let mut forwarders = off_machine_forwarders();
    *forwarders.marked.entry(port).or_default() += 1;
    *forwarders.cable.entry(port).or_default() += 1;
}

/// Stop counting one [`mark_cable_forwarder`] as a cable once its forwarder is gone.
/// The off-machine mark stays for the life of the process, as a LAN proxy's does:
/// a stored connection that withheld the bearer from that port must not get it
/// back at the next re-check, when whatever binds the freed port could receive it.
/// That fails closed.
pub fn drop_cable_mark(port: u16) {
    let mut forwarders = off_machine_forwarders();
    if let Some(count) = forwarders.cable.get_mut(&port) {
        *count -= 1;
        if *count == 0 {
            forwarders.cable.remove(&port);
        }
    }
}

/// Whether `port` forwards to a phone over a path that authenticates the phone
/// itself, as last known: an iOS sidecar forward this process holds, or a port on
/// the adb server's last list for a device on a USB cable. What
/// answers there is the phone adb or the sidecar reached, so a pairing over it may
/// be trusted with no code. A LAN proxy or relay tunnel reads `false`, because what
/// answers through those is whatever the network put there. Before trusting a port
/// with a pairing, use [`is_cable_url_now`], which reads the adb list afresh.
pub fn is_cable_forwarder(port: u16) -> bool {
    let forwarders = off_machine_forwarders();
    forwarders.cable.contains_key(&port) || forwarders.adb_cables.contains(&port)
}

/// The port of `url` when it is `127.0.0.1`, the only address adb and the iOS
/// sidecar bind their forwards to.
fn cable_url_port(url: &str) -> Option<u16> {
    let url = url::Url::parse(url).ok()?;
    match url.host() {
        Some(url::Host::Ipv4(ip)) if ip == std::net::Ipv4Addr::LOCALHOST => {
            url.port_or_known_default()
        }
        _ => None,
    }
}

/// Whether `url` is `127.0.0.1` on a [`is_cable_forwarder`] port, as last known.
pub fn is_cable_url(url: &str) -> bool {
    cable_url_port(url).is_some_and(is_cable_forwarder)
}

/// Whether `url` reaches a phone over a cable now: `127.0.0.1` on a port the iOS
/// sidecar holds, or on a port the adb server lists for a USB device when read now
/// (an `adb connect` over Wi-Fi is not a cable). An adb list
/// that cannot be read counts no adb forward, since a forward whose phone was
/// unplugged leaves its port free for anything to bind.
pub async fn is_cable_url_now(url: &str) -> bool {
    let Some(port) = cable_url_port(url) else {
        return false;
    };
    if off_machine_forwarders().cable.contains_key(&port) {
        return true;
    }
    crate::mcp::adb_helper::refresh_adb_listed_forwarders().await
        && off_machine_forwarders().adb_cables.contains(&port)
}

/// Number a read of the adb server's list, taken before it starts.
pub fn next_adb_list_read() -> u64 {
    static READS: AtomicU64 = AtomicU64::new(0);
    READS.fetch_add(1, Ordering::Relaxed) + 1
}

/// Replace the ports the adb server last listed as forwarding off this machine,
/// and the subset of them that are USB cables, unless a read started after this one
/// has already answered.
pub fn set_adb_listed_forwarders(read: u64, ports: BTreeSet<u16>, cables: BTreeSet<u16>) {
    let mut forwarders = off_machine_forwarders();
    if read > forwarders.adb_list_read {
        forwarders.adb_list_read = read;
        forwarders.adb_listed = ports;
        forwarders.adb_cables = cables;
    }
}

/// Forgets a mark when a test ends, while the port is still bound, so it is not
/// left marked for whichever later test the OS hands it to.
#[cfg(test)]
pub(crate) struct ForgetForwarder(pub(crate) u16);

#[cfg(test)]
impl Drop for ForgetForwarder {
    fn drop(&mut self) {
        unmark_off_machine_forwarder(self.0);
    }
}

/// [`ForgetForwarder`] for a [`mark_cable_forwarder`] mark.
#[cfg(test)]
pub(crate) struct ForgetCableForwarder(pub(crate) u16);

#[cfg(test)]
impl Drop for ForgetCableForwarder {
    fn drop(&mut self) {
        drop_cable_mark(self.0);
        unmark_off_machine_forwarder(self.0);
    }
}

/// Whether `port` is marked, or was on the adb server's last list.
pub fn is_off_machine_forwarder(port: u16) -> bool {
    let forwarders = off_machine_forwarders();
    forwarders.marked.contains_key(&port) || forwarders.adb_listed.contains(&port)
}

#[cfg(test)]
mod off_machine_forwarder_tests {
    use super::{
        is_off_machine_forwarder, mark_off_machine_forwarder, next_adb_list_read,
        set_adb_listed_forwarders, unmark_off_machine_forwarder,
    };

    /// One forwarder letting a port go leaves a second one's mark in place. The
    /// port is held bound so no peer test can be handed it.
    #[test]
    fn a_mark_survives_another_forwarder_letting_the_port_go() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        mark_off_machine_forwarder(port);
        mark_off_machine_forwarder(port);
        unmark_off_machine_forwarder(port);
        assert!(is_off_machine_forwarder(port));
        unmark_off_machine_forwarder(port);
        assert!(!is_off_machine_forwarder(port));
    }

    /// A read that started first and answered last does not replace a newer answer.
    /// The ambient's lock keeps this list away from the door tests, which read it.
    #[test]
    fn an_older_adb_list_read_does_not_land_over_a_newer_one() {
        let _ambient = crate::test_env::isolated_ambient();
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        let older = next_adb_list_read();
        let newer = next_adb_list_read();
        set_adb_listed_forwarders(newer, [port].into(), Default::default());
        set_adb_listed_forwarders(older, Default::default(), Default::default());
        assert!(is_off_machine_forwarder(port));
        set_adb_listed_forwarders(next_adb_list_read(), Default::default(), Default::default());
    }

    /// A LAN proxy's or relay tunnel's mark makes a port off-machine but not a
    /// cable. A cable mark makes it both, at `127.0.0.1` only (adb and the iOS
    /// sidecar bind nothing else). Dropping it leaves the port off-machine.
    #[test]
    fn only_a_cable_mark_makes_a_cable() {
        use super::{drop_cable_mark, is_cable_forwarder, is_cable_url, mark_cable_forwarder};
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();

        mark_off_machine_forwarder(port);
        assert!(is_off_machine_forwarder(port));
        assert!(!is_cable_forwarder(port));
        assert!(!is_cable_url(&format!("http://127.0.0.1:{port}")));
        unmark_off_machine_forwarder(port);

        mark_cable_forwarder(port);
        assert!(is_off_machine_forwarder(port) && is_cable_forwarder(port));
        assert!(is_cable_url(&format!("http://127.0.0.1:{port}/ui-bridge")));
        for host in ["localhost", "[::1]", "127.0.0.2", "192.168.1.20"] {
            assert!(
                !is_cable_url(&format!("http://{host}:{port}/ui-bridge")),
                "{host}"
            );
        }
        assert!(!is_cable_url("not a url"));
        drop_cable_mark(port);
        assert!(is_off_machine_forwarder(port) && !is_cable_forwarder(port));
        unmark_off_machine_forwarder(port);
        assert!(!is_off_machine_forwarder(port));
    }
}

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

    #[error("iOS sidecar error: {0}")]
    IosSidecar(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
