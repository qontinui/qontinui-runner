//! iOS transport (Plan 2).
//!
//! Apple uses proprietary USB/network protocols (usbmux, Lockdown, RemoteXPC)
//! for device control — there is no ADB-equivalent pure-Rust library covering
//! these, so we run `pymobiledevice3` as an out-of-process Python sidecar and
//! talk to it over localhost HTTP. The sidecar lives in
//! `qontinui-runner/ios_bridge/` and is spawned on demand.
//!
//! Lifecycle: `IosTransport::start_service` spawns the child, reads its first
//! stdout line to discover the bound port, then keeps the `Child` handle so
//! we can kill it on shutdown.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::TransportError;

// ----------------------------------------------------------------------------
// Types
// ----------------------------------------------------------------------------

/// One entry from `/devices` on the sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDevice {
    pub udid: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub product_type: Option<String>, // e.g. "iPhone14,2"
    #[serde(default)]
    pub product_version: Option<String>, // e.g. "17.4"
    #[serde(default)]
    pub paired: bool,
    pub connection: String, // "usb" | "network"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListDevicesResponse {
    devices: Vec<IosDevice>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardResponse {
    local_port: u16,
    #[serde(default)]
    _udid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SidecarHandshake {
    port: u16,
    #[allow(dead_code)]
    service: String,
}

// ----------------------------------------------------------------------------
// IosTransport
// ----------------------------------------------------------------------------

/// Owns the `ios_bridge` sidecar child and exposes its endpoints to Rust.
pub struct IosTransport {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    child: Option<Child>,
    port: Option<u16>,
    /// The forward ports this sidecar opened, each marked as a cable until it stops.
    forwards: Vec<u16>,
    /// Counts sidecar starts, so a watcher for an earlier child stops watching.
    generation: u64,
}

impl Inner {
    /// Forget a sidecar that is gone, and stop counting its forwards as cables:
    /// their ports are free for anything to bind. They stay off-machine, so a
    /// connection that withheld the bearer from one never gets it back.
    fn forget_child(&mut self) {
        self.child = None;
        self.port = None;
        for port in std::mem::take(&mut self.forwards) {
            super::drop_cable_mark(port);
        }
    }
}

/// How often a running sidecar is checked for having exited on its own, which
/// frees its forward ports. A forward to a sidecar that died is a cable for at
/// most this long.
const SIDECAR_EXIT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

impl IosTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Spawn the Python sidecar. Returns once the service has printed its
    /// port handshake line. Idempotent — if already running, returns the
    /// existing port.
    pub async fn start_service(&self) -> Result<u16, TransportError> {
        self.start_service_generation().await.map(|(port, _)| port)
    }

    /// [`Self::start_service`], also returning which start the running sidecar is.
    async fn start_service_generation(&self) -> Result<(u16, u64), TransportError> {
        {
            let mut inner = self.inner.lock().await;
            // A sidecar that exited on its own freed its forward ports.
            if inner
                .child
                .as_mut()
                .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
            {
                warn!("ios_bridge sidecar exited; its forwards are gone");
                inner.forget_child();
            }
            if let Some(port) = inner.port {
                if inner.child.is_some() {
                    return Ok((port, inner.generation));
                }
            }
        }

        let ios_bridge_dir = resolve_ios_bridge_dir()?;
        let python = resolve_python_bin();

        info!(
            python = %python.display(),
            dir = %ios_bridge_dir.display(),
            "spawning ios_bridge sidecar"
        );

        let mut cmd = Command::new(&python);
        cmd.arg("-m")
            .arg("ios_bridge")
            .current_dir(ios_bridge_dir.parent().unwrap_or(&ios_bridge_dir))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Hide console window on Windows (matches the runner's other spawns).
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| TransportError::IosSidecar(format!("python spawn: {e}")))?;

        // Read the first stdout line — sidecar prints a JSON handshake there.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::IosSidecar("no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();

        let port = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                debug!(target: "ios_bridge.stdout", %line);
                if let Ok(hs) = serde_json::from_str::<SidecarHandshake>(line) {
                    return Ok::<u16, TransportError>(hs.port);
                }
            }
            Err(TransportError::IosSidecar(
                "sidecar closed stdout before handshake".into(),
            ))
        })
        .await
        .map_err(|_| TransportError::IosSidecar("sidecar handshake timeout".into()))??;

        // Keep draining stdout so the pipe buffer doesn't fill and block.
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(target: "ios_bridge.stdout", %line);
            }
        });

        let mut inner = self.inner.lock().await;
        // Another start finished while this one spawned: keep that sidecar, whose
        // forwards are recorded against its generation, and drop this one, which
        // `kill_on_drop` ends.
        if let Some(running) = inner.child.as_mut() {
            let still_running = matches!(running.try_wait(), Ok(None));
            match inner.port {
                Some(existing) if still_running => {
                    info!(
                        port = existing,
                        "ios_bridge sidecar already running; dropping the second start"
                    );
                    return Ok((existing, inner.generation));
                }
                _ => inner.forget_child(),
            }
        }
        inner.child = Some(child);
        inner.port = Some(port);
        inner.generation += 1;
        let generation = inner.generation;
        drop(inner);
        self.watch_for_exit(generation);
        info!(port, "ios_bridge sidecar ready");
        Ok((port, generation))
    }

    /// Drop the forwards of the sidecar started as `generation` soon after it
    /// exits on its own, rather than at the next start.
    fn watch_for_exit(&self, generation: u64) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SIDECAR_EXIT_POLL).await;
                let mut inner = inner.lock().await;
                if inner.generation != generation {
                    return;
                }
                let Some(child) = inner.child.as_mut() else {
                    return;
                };
                if !matches!(child.try_wait(), Ok(None)) {
                    warn!("ios_bridge sidecar exited; its forwards are gone");
                    inner.forget_child();
                    return;
                }
            }
        });
    }

    /// Kill the sidecar. Safe to call when not running. Its forward ports are then
    /// free for anything to bind, so they stop counting as cables.
    pub async fn stop_service(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            if let Err(e) = child.kill().await {
                warn!(error = %e, "failed to kill ios_bridge child");
            }
        }
        inner.forget_child();
    }

    /// Current sidecar port, if started.
    pub async fn port(&self) -> Option<u16> {
        self.inner.lock().await.port
    }

    /// List USB + Bonjour iOS devices via the sidecar.
    pub async fn list_devices(&self) -> Result<Vec<IosDevice>, TransportError> {
        let port = self.start_service().await?;
        let url = format!("http://127.0.0.1:{port}/devices");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| TransportError::IosSidecar(e.to_string()))?;
        let resp: ListDevicesResponse = client
            .get(&url)
            .send()
            .await
            .map_err(|e| TransportError::IosSidecar(format!("list_devices: {e}")))?
            .json()
            .await
            .map_err(|e| TransportError::IosSidecar(format!("list_devices decode: {e}")))?;
        Ok(resp.devices)
    }

    /// Open a local TCP forward for a device port. Returns the local port.
    ///
    /// The far end is the phone, and the sidecar holds the port while it runs, so
    /// the port is marked as a cable. The sidecar has no close call: the mark is
    /// dropped when it stops or exits. It is marked under the same lock that
    /// records it, and only while the sidecar that opened it still runs, so a stop
    /// in between leaves no mark that nothing would drop.
    pub async fn forward_port(&self, udid: &str, device_port: u16) -> Result<u16, TransportError> {
        let (port, generation) = self.start_service_generation().await?;
        let local_port = request_forward(port, udid, device_port).await?;
        if !record_forward(&mut *self.inner.lock().await, generation, local_port) {
            return Err(TransportError::ForwardFailed {
                device_id: udid.to_string(),
                reason: "the ios_bridge sidecar stopped while the forward was opened".into(),
            });
        }
        Ok(local_port)
    }

    /// Initiate pairing (triggers the 'Trust this computer' dialog).
    pub async fn initiate_pair(&self, udid: &str) -> Result<(), TransportError> {
        let port = self.start_service().await?;
        let url = format!("http://127.0.0.1:{port}/pair/{udid}");
        reqwest::Client::new()
            .post(&url)
            .send()
            .await
            .map_err(|e| TransportError::PairingError(e.to_string()))?;
        Ok(())
    }
}

impl Default for IosTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Mark `local_port` as a cable and record it, when the sidecar started as
/// `generation` still runs. `false` when it stopped or was replaced meanwhile.
fn record_forward(inner: &mut Inner, generation: u64, local_port: u16) -> bool {
    if inner.generation != generation || inner.port.is_none() {
        return false;
    }
    super::mark_cable_forwarder(local_port);
    inner.forwards.push(local_port);
    true
}

/// Ask the sidecar on `sidecar_port` to forward `device_port`.
async fn request_forward(
    sidecar_port: u16,
    udid: &str,
    device_port: u16,
) -> Result<u16, TransportError> {
    let url = format!("http://127.0.0.1:{sidecar_port}/forward");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| TransportError::IosSidecar(e.to_string()))?;
    let body = serde_json::json!({ "udid": udid, "devicePort": device_port });
    let resp: ForwardResponse = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| TransportError::ForwardFailed {
            device_id: udid.to_string(),
            reason: e.to_string(),
        })?
        .json()
        .await
        .map_err(|e| TransportError::ForwardFailed {
            device_id: udid.to_string(),
            reason: format!("decode: {e}"),
        })?;
    Ok(resp.local_port)
}

// ----------------------------------------------------------------------------
// Resolution helpers
// ----------------------------------------------------------------------------

/// Locate the `ios_bridge/` directory (which contains `__main__.py`).
///
/// Search order:
/// 1. `IOS_BRIDGE_DIR` env var
/// 2. `<runner repo>/ios_bridge/` via current exe's path
/// 3. `./ios_bridge/` relative to CWD
fn resolve_ios_bridge_dir() -> Result<PathBuf, TransportError> {
    if let Ok(env_dir) = std::env::var("IOS_BRIDGE_DIR") {
        let p = PathBuf::from(env_dir);
        if p.join("__main__.py").exists() {
            return Ok(p);
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        // qontinui-runner/src-tauri/target/.../runner.exe -> ../../../../ios_bridge
        let mut p = exe_path.clone();
        for _ in 0..5 {
            if let Some(parent) = p.parent() {
                let candidate = parent.join("ios_bridge");
                if candidate.join("__main__.py").exists() {
                    return Ok(candidate);
                }
                p = parent.to_path_buf();
            } else {
                break;
            }
        }
    }

    let cwd_candidate = std::env::current_dir()
        .ok()
        .map(|d| d.join("ios_bridge"))
        .filter(|p| p.join("__main__.py").exists());
    if let Some(p) = cwd_candidate {
        return Ok(p);
    }

    Err(TransportError::IosSidecar(
        "ios_bridge directory not found (set IOS_BRIDGE_DIR or place ios_bridge/ next to runner exe)".into(),
    ))
}

fn resolve_python_bin() -> PathBuf {
    if let Ok(env_py) = std::env::var("IOS_BRIDGE_PYTHON") {
        return PathBuf::from(env_py);
    }
    // `python` works on Windows (Python launcher) and Unix.
    PathBuf::from(if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    })
}

#[cfg(test)]
mod tests {
    use super::{record_forward, request_forward, Inner};
    use crate::mcp::transport::{
        is_cable_forwarder, is_off_machine_forwarder, unmark_off_machine_forwarder,
    };

    /// The port the sidecar hands back is marked as a cable forwarder while the
    /// sidecar that opened it runs. When it stops the port stops being a cable but
    /// stays off-machine, so a connection that withheld the bearer from it never
    /// gets it back. A sidecar that stopped or was replaced before the answer came
    /// back marks nothing. The port is held bound so no peer test can be handed it.
    #[tokio::test]
    async fn a_forward_port_from_the_sidecar_is_a_cable_while_the_sidecar_runs() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let forward_port = held.local_addr().expect("local addr").port();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sidecar_port = listener.local_addr().unwrap().port();
        let sidecar = axum::Router::new().route(
            "/forward",
            axum::routing::post(move || async move {
                axum::Json(serde_json::json!({ "localPort": forward_port }))
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, sidecar).await;
        });

        let opened = request_forward(sidecar_port, "udid-under-test", 9876)
            .await
            .expect("the sidecar answers");
        assert_eq!(opened, forward_port);

        let mut inner = Inner {
            port: Some(sidecar_port),
            generation: 2,
            ..Inner::default()
        };
        assert!(
            !record_forward(&mut inner, 1, opened),
            "a replaced sidecar's forward was kept"
        );
        assert!(!is_cable_forwarder(forward_port));

        assert!(record_forward(&mut inner, 2, opened));
        assert!(is_off_machine_forwarder(forward_port));
        assert!(is_cable_forwarder(forward_port));

        inner.forget_child();
        assert!(
            is_off_machine_forwarder(forward_port),
            "a stopped sidecar's forward reads as loopback, so a withheld bearer would be sent"
        );
        assert!(!is_cable_forwarder(forward_port));

        let mut stopped = Inner {
            generation: 2,
            ..Inner::default()
        };
        assert!(
            !record_forward(&mut stopped, 2, opened),
            "a stopped sidecar's forward was kept"
        );
        assert!(!is_cable_forwarder(forward_port));

        unmark_off_machine_forwarder(forward_port);
        assert!(!is_off_machine_forwarder(forward_port));
    }
}
