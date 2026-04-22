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
}

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
        {
            let inner = self.inner.lock().await;
            if let Some(port) = inner.port {
                if inner.child.is_some() {
                    return Ok(port);
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
        inner.child = Some(child);
        inner.port = Some(port);
        info!(port, "ios_bridge sidecar ready");
        Ok(port)
    }

    /// Kill the sidecar. Safe to call when not running.
    pub async fn stop_service(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            if let Err(e) = child.kill().await {
                warn!(error = %e, "failed to kill ios_bridge child");
            }
        }
        inner.port = None;
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
    pub async fn forward_port(&self, udid: &str, device_port: u16) -> Result<u16, TransportError> {
        let port = self.start_service().await?;
        let url = format!("http://127.0.0.1:{port}/forward");
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
