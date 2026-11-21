//! The USB/ADB discovery scanner: every `period` it lists `adb devices`,
//! reverses the runner's HTTP API port to each attached phone, forwards the
//! UI Bridge port of each new one and registers it with the
//! [`PhysicalDeviceRegistry`], and tears both down for phones that left.
//!
//! It runs under the panic supervisor ([`spawn_adb_scanner`]). A supervisor
//! restart calls the factory again, so everything a run must share with the
//! previous one is built ONCE, before the spawn: the [`UsbTransport`] is
//! published into `AppState::usb_transport` first, and every run drives a
//! clone of that one transport. A clone shares its forward and reverse maps,
//! so a rebuilt run and the shutdown `release_all` (`mcp/ai_session.rs`) see
//! the same `adb forward` / `adb reverse` entries.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OnceCell;

use crate::commands::AppState;
use crate::mcp::physical_device::PhysicalDeviceRegistry;
use crate::mcp::transport::usb::UsbTransport;
use crate::worker_supervisor::Heartbeat;

/// The `worker_supervisor` registry name of the loop [`spawn_adb_scanner`]
/// starts, as `GET /health` `supervised_workers` lists it.
pub const ADB_SCANNER_WORKER: &str = "usb.adb_scanner";

/// How long the scanner waits before its first pass, for ADB and the other
/// services to initialize.
pub const ADB_SCANNER_START_DELAY: Duration = Duration::from_secs(5);

/// The interval between passes.
pub const ADB_SCANNER_PERIOD: Duration = Duration::from_secs(30);

/// Publish the process's one [`UsbTransport`] into `cell` (a no-op when it is
/// already there) and return a clone of the published one.
fn published_transport(cell: &OnceCell<UsbTransport>) -> UsbTransport {
    let _ = cell.set(UsbTransport::new());
    match cell.get() {
        Some(transport) => transport.clone(),
        // `set` above leaves the cell populated, by this call or an earlier one.
        None => unreachable!("the UsbTransport cell is set before it is read"),
    }
}

/// Run the USB/ADB scanner for the life of the process: wait `start_delay`,
/// then scan every `period`.
///
/// The transport is published into `app_state.usb_transport` here, before the
/// spawn — the shutdown handler (`mcp/ai_session.rs`) calls `release_all` on it
/// so this process's `adb forward` entries don't linger (graceful-only; a
/// force-kill still leaks; plan adb-forwarder-port.md §1.6a). Every run of the
/// supervised loop drives a clone of that same transport.
pub fn spawn_adb_scanner(
    app_state: Arc<AppState>,
    registry: Arc<PhysicalDeviceRegistry>,
    start_delay: Duration,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    let transport = published_transport(&app_state.usb_transport);
    let factory = scanner_factory(transport, app_state, registry, start_delay, period);
    crate::worker_supervisor::spawn_supervised_with_heartbeat(ADB_SCANNER_WORKER, move |hb| {
        factory(hb).run()
    })
}

/// The supervisor's factory: each call builds one run around a clone of the
/// SAME transport.
fn scanner_factory(
    transport: UsbTransport,
    app_state: Arc<AppState>,
    registry: Arc<PhysicalDeviceRegistry>,
    start_delay: Duration,
    period: Duration,
) -> impl Fn(Heartbeat) -> ScannerRun + Send + 'static {
    move |hb| ScannerRun {
        transport: transport.clone(),
        app_state: app_state.clone(),
        registry: registry.clone(),
        start_delay,
        period,
        hb,
    }
}

/// One run of the scanner loop.
struct ScannerRun {
    transport: UsbTransport,
    app_state: Arc<AppState>,
    registry: Arc<PhysicalDeviceRegistry>,
    start_delay: Duration,
    period: Duration,
    hb: Heartbeat,
}

impl ScannerRun {
    async fn run(self) {
        let ScannerRun {
            transport: usb_transport,
            app_state,
            registry,
            start_delay,
            period,
            hb,
        } = self;

        // Wait for ADB and other services to initialize
        tokio::time::sleep(start_delay).await;

        let mobile_settings = crate::settings::get_mobile_settings();

        let mut interval = tokio::time::interval(period);

        loop {
            interval.tick().await;
            hb.tick();

            let devices = usb_transport.scan_devices().await;

            // Register newly connected physical devices.
            //
            // Re-establishment after transport eviction: the dedup below
            // skips a device only when it already has a live USB transport
            // entry in the registry. If the health monitor removed the
            // USB transport (3 consecutive failures → `remove_transport`
            // leaves the device entry with `transports: []` and
            // `health_state: Unreachable`), the next scanner pass must
            // retry `establish_forward` — otherwise the device is
            // permanently dead in the registry while still being a live
            // adb target. Previously the dedup checked `get_device(...).is_some()`,
            // which also matched the empty-transport "stuck" state and
            // produced `transports: []` + `healthState: unreachable`
            // responses on /ui-bridge/devices even though raw probes to
            // localhost:<ui_bridge_port> succeeded.
            for (device_id, status, model) in &devices {
                if status != "device" {
                    continue;
                }
                if device_id.starts_with("emulator-") {
                    continue; // Skip emulators — handled by app_discovery
                }

                // Reverse the runner HTTP API port back to the device so the
                // qontinui-mobile app's data path can reach the runner at
                // `localhost:<port>` on the phone. Without this, USB-attached
                // phones see "Network request failed". This is independent of
                // UI Bridge discovery below — the data path is needed whenever
                // a physical device is attached, registered or not — so it runs
                // before the already-registered skip. Idempotent + only logged
                // on first install per serial (re-runs are skipped via the
                // active_reverses map).
                if usb_transport
                    .active_reverses
                    .lock()
                    .await
                    .get(device_id)
                    .is_none()
                {
                    let runner_port = app_state.api_port.load(Ordering::Relaxed);
                    if let Err(e) = usb_transport
                        .establish_reverse(device_id, runner_port)
                        .await
                    {
                        tracing::debug!(
                            "Failed to establish ADB reverse for {} (port {}): {}",
                            device_id,
                            runner_port,
                            e
                        );
                    }
                }

                // Skip devices that already have a live USB transport.
                // A device entry with empty `transports` (or only
                // non-USB transports) falls through to re-establishment.
                if registry
                    .has_transport_kind(device_id, crate::mcp::transport::TransportKind::Usb)
                    .await
                {
                    continue;
                }

                // Attempt ADB port forward to the UI Bridge port
                match usb_transport
                    .establish_forward(device_id, mobile_settings.ui_bridge_port)
                    .await
                {
                    Ok(local_port) => {
                        // Quick health check, direct: the port is this machine's.
                        let client = reqwest::Client::builder()
                            .no_proxy()
                            .timeout(std::time::Duration::from_secs(2))
                            .build()
                            .unwrap_or_else(|_| reqwest::Client::new());
                        let base_url = format!("http://127.0.0.1:{}", local_port);
                        let url = format!("{base_url}/ui-bridge/health");
                        if let Ok(resp) = client.get(&url).send().await {
                            // Over the forward the phone admits this runner as on the
                            // phone itself, and so hands over its pairing token with no
                            // code: fetch it now, so the same phone is reachable over
                            // Wi-Fi once the cable is out.
                            let gate =
                                resp.json::<serde_json::Value>()
                                    .await
                                    .ok()
                                    .and_then(|health| {
                                        crate::mcp::transport::pairing::DeviceGate::from_health(
                                            &health,
                                        )
                                    });
                            if let Some(gate) = gate.filter(|gate| gate.admits_as_local()) {
                                if let Some(ui_bridge_id) = gate.device_id {
                                    crate::mcp::transport::pairing::spawn_token_fetch(
                                        base_url.clone(),
                                        ui_bridge_id,
                                        true,
                                    );
                                }
                            }
                            let now = chrono::Utc::now().timestamp_millis();
                            let info = crate::mcp::physical_device::PhysicalDeviceInfo {
                                id: device_id.clone(),
                                os: crate::mcp::transport::DeviceOs::Android,
                                device_kind: "physical".to_string(),
                                model: model.clone(),
                                app_id: None,
                                ui_bridge_version: None,
                                first_seen_at: now,
                            };
                            let transport = crate::mcp::physical_device::ActiveTransport {
                                kind: crate::mcp::transport::TransportKind::Usb,
                                proxy_url: format!("http://127.0.0.1:{}", local_port),
                                established_at: now,
                                last_healthy_at: now,
                                fail_count: 0,
                            };
                            registry.register(info, transport).await;
                            tracing::info!(
                                "USB device registered: {} (local port {})",
                                device_id,
                                local_port
                            );
                        } else {
                            // No UI Bridge on device — release the forward
                            let _ = usb_transport.release_forward(device_id).await;
                            tracing::debug!(
                                "USB device {} reachable via ADB but no UI Bridge (port {})",
                                device_id,
                                local_port
                            );
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Failed to establish ADB forward for {}: {}", device_id, e);
                    }
                }
            }

            // Clean up devices that are no longer connected via USB
            let registered = registry.list_all().await;
            for device in registered {
                let has_usb = device
                    .transports
                    .iter()
                    .any(|t| t.kind == crate::mcp::transport::TransportKind::Usb);
                if !has_usb {
                    continue;
                }
                let still_connected = devices
                    .iter()
                    .any(|(id, status, _)| id == &device.info.id && status == "device");
                if !still_connected {
                    registry
                        .remove_transport(
                            &device.info.id,
                            crate::mcp::transport::TransportKind::Usb,
                        )
                        .await;
                    let _ = usb_transport.release_forward(&device.info.id).await;
                    tracing::info!("USB device disconnected: {}", device.info.id);
                }
            }

            // Tear down ADB reverses for devices no longer attached. Reverses
            // are installed for every physical device (even those without a UI
            // Bridge, which never register in the registry above), so they need
            // their own disconnect sweep keyed off the live `adb devices` list.
            let reversed_serials: Vec<String> = {
                let reverses = usb_transport.active_reverses.lock().await;
                reverses.keys().cloned().collect()
            };
            for serial in reversed_serials {
                let still_connected = devices
                    .iter()
                    .any(|(id, status, _)| id == &serial && status == "device");
                if !still_connected {
                    let _ = usb_transport.release_reverse(&serial).await;
                    tracing::info!("USB device reverse released (disconnected): {}", serial);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a supervisor restart does: call the factory a second time. Both
    /// runs, and the cell the shutdown handler reads, hold the SAME forward
    /// map — a rebuilt run that built its own transport would leave the cell
    /// and the loop on different instances.
    #[test]
    fn two_factory_runs_share_the_published_transport() {
        let app_state = AppState::new_for_test();
        let transport = published_transport(&app_state.usb_transport);
        let factory = scanner_factory(
            transport,
            app_state.clone(),
            Arc::new(PhysicalDeviceRegistry::new()),
            ADB_SCANNER_START_DELAY,
            ADB_SCANNER_PERIOD,
        );
        let first = factory(Heartbeat(ADB_SCANNER_WORKER));
        let second = factory(Heartbeat(ADB_SCANNER_WORKER));
        let published = app_state
            .usb_transport
            .get()
            .expect("the transport is published before the spawn");

        assert!(Arc::ptr_eq(
            &first.transport.active_forwards,
            &second.transport.active_forwards
        ));
        assert!(Arc::ptr_eq(
            &first.transport.active_forwards,
            &published.active_forwards
        ));
        assert!(Arc::ptr_eq(
            &second.transport.active_reverses,
            &published.active_reverses
        ));
    }

    /// Publishing twice keeps the first transport rather than replacing it.
    #[test]
    fn publishing_again_returns_the_transport_already_in_the_cell() {
        let cell = OnceCell::new();
        let first = published_transport(&cell);
        let again = published_transport(&cell);
        assert!(Arc::ptr_eq(&first.active_forwards, &again.active_forwards));
    }

    /// The door `create_router` calls publishes the transport and registers the
    /// scanner with the panic supervisor under its `/health` name. The start
    /// delay is long, so no pass touches the developer's adb server.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_adb_scanner_registers_with_the_panic_supervisor() {
        let app_state = AppState::new_for_test();
        let handle = spawn_adb_scanner(
            app_state.clone(),
            Arc::new(PhysicalDeviceRegistry::new()),
            Duration::from_secs(3600),
            ADB_SCANNER_PERIOD,
        );
        assert!(
            app_state.usb_transport.get().is_some(),
            "the transport must be published before the loop starts"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !crate::worker_supervisor::snapshot()
            .iter()
            .any(|row| row.name == ADB_SCANNER_WORKER)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "spawn_adb_scanner never registered {ADB_SCANNER_WORKER}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.abort();
    }
}
