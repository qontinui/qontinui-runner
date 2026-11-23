//! mDNS-based LAN device scanner.
//!
//! Devices running UI Bridge advertise themselves via mDNS under the service
//! type `_uibridge._tcp.local.`  This module consumes those advertisements
//! and emits `MdnsEvent`s so the physical device registry can add LAN
//! transports automatically.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ============================================================================
// Constants
// ============================================================================

/// mDNS service type advertised by UI Bridge instances on the local network.
pub const MDNS_SERVICE_TYPE: &str = "_uibridge._tcp.local.";

/// Plan 1C: the runner also advertises itself under this service type so phones
/// can find the runner. Separate from the device-side `_uibridge._tcp.local.`
/// so mutual discovery doesn't cause a feedback loop.
pub const RUNNER_MDNS_SERVICE_TYPE: &str = "_qontinui._tcp.local.";

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

/// Scans the local network for UI Bridge services via mDNS.
pub struct MdnsScanner {
    daemon: Option<ServiceDaemon>,
}

impl MdnsScanner {
    /// Create a new scanner.  If the mDNS daemon cannot be initialised (e.g.
    /// WinSock2 not available), the scanner operates in no-op mode and logs a
    /// warning.
    pub fn new() -> Self {
        match ServiceDaemon::new() {
            Ok(daemon) => Self {
                daemon: Some(daemon),
            },
            Err(e) => {
                warn!(
                    "Failed to create mDNS daemon: {}. LAN discovery disabled.",
                    e
                );
                Self { daemon: None }
            }
        }
    }

    /// Start scanning for UI Bridge services on the local network.
    ///
    /// Sends [`MdnsEvent`]s through `event_tx` as devices appear and
    /// disappear.  Events are delivered from a dedicated OS thread so the
    /// receiver must handle them from an async context (e.g. via
    /// `mpsc::Receiver::recv().await`).
    ///
    /// If the mDNS daemon is not available the method logs a message and
    /// returns immediately without spawning a thread.
    pub fn start(&self, event_tx: mpsc::Sender<MdnsEvent>) {
        let daemon = match &self.daemon {
            Some(d) => d,
            None => {
                info!(
                    "mDNS daemon not available, skipping LAN discovery. \
                     Use POST /ui-bridge/devices/register-lan to add LAN devices manually."
                );
                return;
            }
        };

        let receiver = match daemon.browse(MDNS_SERVICE_TYPE) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Failed to start mDNS browse for {}: {}",
                    MDNS_SERVICE_TYPE, e
                );
                return;
            }
        };

        info!("mDNS scanner started, browsing for {}", MDNS_SERVICE_TYPE);

        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let txt = info.get_properties();
                        let mut records: HashMap<String, String> = HashMap::new();
                        for prop in txt.iter() {
                            records.insert(prop.key().to_string(), prop.val_str().to_string());
                        }

                        let device_id = records
                            .get("device_id")
                            .cloned()
                            .unwrap_or_else(|| info.get_fullname().to_string());

                        let addresses: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                        let port = info.get_port();

                        debug!("mDNS discovered: {} at {:?}:{}", device_id, addresses, port);

                        let device_info = MdnsDeviceInfo {
                            device_id,
                            addresses,
                            port,
                            txt_records: records,
                        };

                        if event_tx
                            .blocking_send(MdnsEvent::Discovered(device_info))
                            .is_err()
                        {
                            break; // Receiver dropped — stop scanning
                        }
                    }
                    ServiceEvent::ServiceRemoved(_ty, fullname) => {
                        debug!("mDNS service removed: {}", fullname);
                        if event_tx
                            .blocking_send(MdnsEvent::Removed(fullname))
                            .is_err()
                        {
                            break;
                        }
                    }
                    ServiceEvent::SearchStarted(_) => {
                        debug!("mDNS search started for {}", MDNS_SERVICE_TYPE);
                    }
                    ServiceEvent::SearchStopped(_) => {
                        debug!("mDNS search stopped for {}", MDNS_SERVICE_TYPE);
                    }
                    _ => {}
                }
            }

            debug!("mDNS scanner thread exiting");
        });
    }

    /// Stop browsing for the UI Bridge service type.
    pub fn stop(&self) {
        if let Some(daemon) = &self.daemon {
            if let Err(e) = daemon.stop_browse(MDNS_SERVICE_TYPE) {
                debug!("mDNS stop_browse returned: {}", e);
            }
        }
    }
}

impl MdnsScanner {
    /// Plan 1C: register the runner itself under `RUNNER_MDNS_SERVICE_TYPE` so
    /// phones on the same LAN can discover it.
    ///
    /// `instance_name` should be unique per runner (hostname or machine id).
    /// `port` is the MCP API port the runner is listening on.
    /// `properties` become TXT records (device_id, version, auth_mode, …).
    pub fn register_runner(
        &self,
        instance_name: &str,
        port: u16,
        properties: HashMap<String, String>,
    ) -> Result<(), String> {
        let daemon = self.daemon.as_ref().ok_or("mDNS daemon unavailable")?;

        // ServiceInfo::new expects host IPs; use local hostname so clients resolve.
        let host = hostname::get()
            .ok()
            .and_then(|h| h.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "qontinui".to_string());
        let host = format!("{}.local.", host);

        let svc = ServiceInfo::new(
            RUNNER_MDNS_SERVICE_TYPE,
            instance_name,
            &host,
            "",
            port,
            properties,
        )
        .map_err(|e| format!("invalid ServiceInfo: {e}"))?
        .enable_addr_auto();

        daemon
            .register(svc)
            .map_err(|e| format!("mDNS register failed: {e}"))?;
        info!(
            service = RUNNER_MDNS_SERVICE_TYPE,
            instance = instance_name,
            port,
            "runner registered under mDNS"
        );
        Ok(())
    }

    /// Unregister the runner's own advertisement. Best-effort.
    pub fn unregister_runner(&self, instance_name: &str) {
        if let Some(daemon) = &self.daemon {
            let fullname = format!("{}.{}", instance_name, RUNNER_MDNS_SERVICE_TYPE);
            if let Err(e) = daemon.unregister(&fullname) {
                debug!(error = %e, "mDNS unregister returned");
            }
        }
    }

    /// Plan 1C: start scanning and fan out every event as a Tauri event so the
    /// wizard's React UI can update in real-time.
    ///
    /// Events emitted to the frontend:
    /// - `device-discovered` — payload is [`MdnsDeviceEventPayload`]
    /// - `device-removed` — payload is `{ fullname }`
    pub fn start_with_tauri_events(&self, app: tauri::AppHandle) {
        let (tx, mut rx) = mpsc::channel::<MdnsEvent>(64);
        self.start(tx);

        tokio::spawn(async move {
            use tauri::Emitter;
            while let Some(ev) = rx.recv().await {
                match ev {
                    MdnsEvent::Discovered(info) => {
                        let payload = MdnsDeviceEventPayload {
                            device_id: info.device_id,
                            port: info.port,
                            addresses: info.addresses.iter().map(|a| a.to_string()).collect(),
                            txt_records: info.txt_records,
                        };
                        let _ = app.emit("device-discovered", &payload);
                    }
                    MdnsEvent::Removed(fullname) => {
                        let _ = app.emit(
                            "device-removed",
                            serde_json::json!({
                                "fullname": fullname,
                            }),
                        );
                    }
                }
            }
            debug!("mDNS Tauri event bridge ended");
        });
    }
}

/// Payload shape emitted to the frontend on `device-discovered`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MdnsDeviceEventPayload {
    pub device_id: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub txt_records: HashMap<String, String>,
}

impl Default for MdnsScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MdnsScanner {
    fn drop(&mut self) {
        self.stop();
        if let Some(daemon) = self.daemon.take() {
            if let Err(e) = daemon.shutdown() {
                debug!("mDNS daemon shutdown returned: {}", e);
            }
        }
    }
}

// ============================================================================
// The supervised event loop
// ============================================================================

/// The `worker_supervisor` registry name of the loop [`spawn_event_loop`]
/// starts, as `GET /health` `supervised_workers` lists it. Absent from `/health`
/// when LAN discovery is off: the gate sits before the spawn.
pub const MDNS_EVENTS_WORKER: &str = "mdns_scanner.events";

/// How long the event loop waits after boot before it drains its first event.
pub const MDNS_EVENTS_START_DELAY: std::time::Duration = std::time::Duration::from_secs(6);

/// The receiver every run of the event loop drains. Shared, not moved into one
/// run: a receiver moved into a run would die with it, and the browse thread's
/// sender would then have no one left to talk to.
type SharedEvents = std::sync::Arc<tokio::sync::Mutex<mpsc::Receiver<MdnsEvent>>>;

/// Register every UI Bridge instance mDNS discovers as a LAN transport in
/// `registry` — when `settings` enables LAN discovery. Returns `None`, calling
/// nothing and starting nothing, when it does not.
///
/// `start_scanner` builds the scanner and starts its browse thread on the
/// sender it is handed (in production, `MdnsScanner::new()` then
/// `scanner.start(event_tx)`, written at the `create_router` call site, where
/// the boot-task census counts that bare thread). It runs once, here, before
/// the spawn, and the returned scanner is owned by the supervisor's factory for
/// as long as the loop lives: its `Drop` shuts the mDNS daemon down.
///
/// The drain runs under the panic supervisor, so a panic is logged, counted on
/// `/health` and the loop rebuilt after a backoff. A rebuilt run waits
/// `start_delay` again and then drains the SAME receiver, so the browse thread
/// and its daemon are untouched by a panic in the drain. When the browse thread
/// ends (its sender drops), the receiver closes and the run returns, which
/// `/health` records as `exited`. Event-driven, so no heartbeat: an idle LAN is
/// not a wedged loop.
pub fn spawn_event_loop(
    registry: std::sync::Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    settings: &crate::settings::MobileSettings,
    start_delay: std::time::Duration,
    start_scanner: impl FnOnce(mpsc::Sender<MdnsEvent>) -> MdnsScanner,
) -> Option<tokio::task::JoinHandle<()>> {
    if !settings.lan_discovery_enabled {
        debug!("LAN discovery disabled, skipping mDNS scanner");
        return None;
    }
    let (event_tx, event_rx) = mpsc::channel::<MdnsEvent>(32);
    let scanner = start_scanner(event_tx);
    let events: SharedEvents = std::sync::Arc::new(tokio::sync::Mutex::new(event_rx));
    Some(crate::worker_supervisor::spawn_supervised(
        MDNS_EVENTS_WORKER,
        move || {
            // The factory, not a run, owns the scanner: a run that panics must
            // not take the daemon down with it.
            let _daemon = &scanner;
            events_run(events.clone(), registry.clone(), start_delay)
        },
    ))
}

/// One run of the event loop: wait `start_delay`, then register every
/// discovered device until the browse thread's sender drops.
async fn events_run(
    events: SharedEvents,
    registry: std::sync::Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    start_delay: std::time::Duration,
) {
    tokio::time::sleep(start_delay).await;
    // Only one run is ever live, so the lock is uncontended; a run that
    // panicked released it on unwind (a tokio mutex does not poison).
    let mut rx = events.lock().await;
    while let Some(event) = rx.recv().await {
        register_event(&registry, event).await;
    }
    debug!("mDNS event loop: the browse thread's sender is gone — exiting");
}

/// Apply one mDNS event to the physical device registry.
async fn register_event(
    registry: &crate::mcp::physical_device::PhysicalDeviceRegistry,
    event: MdnsEvent,
) {
    match event {
        MdnsEvent::Discovered(info) => {
            let Some(&addr) = info.addresses.first() else {
                return;
            };
            let socket_addr = std::net::SocketAddr::new(addr, info.port);
            info!("mDNS device found: {} at {}", info.device_id, socket_addr);

            // Register as a LAN transport in the physical device registry.
            // We proxy directly to the device's IP/port; no local TCP proxy
            // is needed for same-LAN connections.
            let now = chrono::Utc::now().timestamp_millis();
            // Determine OS from the "platform" TXT record if present
            let os = match info
                .txt_records
                .get("platform")
                .map(|s| s.to_lowercase())
                .as_deref()
            {
                Some("ios") | Some("iphone") | Some("ipad") => crate::mcp::transport::DeviceOs::Ios,
                _ => crate::mcp::transport::DeviceOs::Android,
            };
            let device_info = crate::mcp::physical_device::PhysicalDeviceInfo {
                id: info.device_id.clone(),
                os,
                device_kind: "physical".to_string(),
                model: info.txt_records.get("model").cloned(),
                app_id: info.txt_records.get("app_id").cloned(),
                ui_bridge_version: info.txt_records.get("version").cloned(),
                first_seen_at: now,
            };
            let transport = crate::mcp::physical_device::ActiveTransport {
                kind: crate::mcp::transport::TransportKind::Lan,
                proxy_url: format!("http://{}", socket_addr),
                established_at: now,
                last_healthy_at: now,
                fail_count: 0,
            };
            registry.register(device_info, transport).await;
        }
        MdnsEvent::Removed(name) => {
            debug!("mDNS device removed: {}", name);
            // The health monitor will evict the transport when health
            // checks start failing; no immediate action needed here.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::physical_device::PhysicalDeviceRegistry;
    use crate::settings::MobileSettings;
    use std::sync::Arc;
    use std::time::Duration;

    /// A scanner with no daemon: starting it spawns no thread and touches no
    /// network.
    fn inert_scanner() -> MdnsScanner {
        MdnsScanner { daemon: None }
    }

    fn lan(enabled: bool) -> MobileSettings {
        MobileSettings {
            lan_discovery_enabled: enabled,
            ..MobileSettings::default()
        }
    }

    fn discovered(device_id: &str) -> MdnsEvent {
        MdnsEvent::Discovered(MdnsDeviceInfo {
            device_id: device_id.to_string(),
            addresses: vec![IpAddr::from([192, 0, 2, 7])],
            port: 8087,
            txt_records: HashMap::new(),
        })
    }

    async fn registered(registry: &PhysicalDeviceRegistry, id: &str) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if registry.get_device(id).await.is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// The gate: LAN discovery off builds no scanner and starts nothing, so the
    /// worker is absent from `/health` rather than `exited`.
    #[tokio::test]
    async fn spawn_event_loop_returns_none_when_lan_discovery_is_disabled() {
        let mut started = false;
        let handle = spawn_event_loop(
            Arc::new(PhysicalDeviceRegistry::new()),
            &lan(false),
            MDNS_EVENTS_START_DELAY,
            |_| {
                started = true;
                inert_scanner()
            },
        );
        assert!(handle.is_none());
        assert!(!started, "a disabled gate must not build the scanner");
    }

    /// The door `create_router` calls, with LAN discovery on, registers the
    /// loop with the panic supervisor under its `/health` name.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_event_loop_registers_with_the_panic_supervisor_when_enabled() {
        // Hold the sender, as the browse thread would, so the run does not
        // return on a closed channel.
        let mut sender = None;
        let handle = spawn_event_loop(
            Arc::new(PhysicalDeviceRegistry::new()),
            &lan(true),
            Duration::from_secs(3600),
            |event_tx| {
                sender = Some(event_tx);
                inert_scanner()
            },
        )
        .expect("enabled LAN discovery starts the event loop");
        assert!(
            sender.is_some(),
            "the scanner starter must be handed the sender"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !crate::worker_supervisor::snapshot()
            .iter()
            .any(|row| row.name == MDNS_EVENTS_WORKER)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "spawn_event_loop never registered {MDNS_EVENTS_WORKER}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.abort();
    }

    /// What a supervisor restart does: run the loop a second time. Both runs
    /// drain the one shared receiver, so the browse thread's sender still
    /// reaches the rebuilt run — a receiver moved into the first run would have
    /// died with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rebuilt_run_drains_the_same_receiver() {
        let (tx, rx) = mpsc::channel::<MdnsEvent>(32);
        let events: SharedEvents = Arc::new(tokio::sync::Mutex::new(rx));
        let registry = Arc::new(PhysicalDeviceRegistry::new());

        let first = tokio::spawn(events_run(events.clone(), registry.clone(), Duration::ZERO));
        tx.send(discovered("first-device"))
            .await
            .expect("receiver alive");
        assert!(
            registered(&registry, "first-device").await,
            "the first run never registered its device"
        );
        // A panic ends the run; aborting it is the same loss of the run.
        first.abort();
        let _ = first.await;

        let second = tokio::spawn(events_run(events.clone(), registry.clone(), Duration::ZERO));
        tx.send(discovered("second-device"))
            .await
            .expect("the shared receiver outlives the first run");
        assert!(
            registered(&registry, "second-device").await,
            "the rebuilt run never registered its device"
        );
        second.abort();
    }

    /// The browse thread ending drops its sender: the run returns, which the
    /// supervisor records as `exited` rather than restarting it.
    #[tokio::test]
    async fn a_run_returns_when_the_browse_thread_sender_drops() {
        let (tx, rx) = mpsc::channel::<MdnsEvent>(32);
        drop(tx);
        let run = events_run(
            Arc::new(tokio::sync::Mutex::new(rx)),
            Arc::new(PhysicalDeviceRegistry::new()),
            Duration::ZERO,
        );
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("the run must return once the sender is gone");
    }
}
