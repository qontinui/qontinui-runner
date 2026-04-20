//! Rathole client-process supervisor.
//!
//! Rathole (https://github.com/rapiz1/rathole) is distributed as a standalone
//! binary only, so we treat it like any other bundled helper: write the config
//! to a temp file, spawn the binary, and hold onto the child handle.
//!
//! Binary resolution order:
//! 1. `RATHOLE_BIN` env var (absolute path)
//! 2. `MobileSettings::rathole_path` (future — not yet wired)
//! 3. `rathole` / `rathole.exe` alongside the runner executable
//! 4. `rathole` on `PATH`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use super::config::RatholeConfig;

// ----------------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("rathole binary not found (set RATHOLE_BIN or add rathole to PATH)")]
    BinaryNotFound,

    #[error("failed to spawn rathole: {0}")]
    SpawnFailed(String),

    #[error("failed to write tunnel config: {0}")]
    ConfigIo(String),

    #[error("rathole client is not running")]
    NotRunning,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ----------------------------------------------------------------------------
// Client
// ----------------------------------------------------------------------------

/// Manages a single rathole client subprocess.
///
/// The client owns a temp config file and the spawned `Child`. Calling
/// `start()` while already running is a no-op after a stale-process check.
#[derive(Default)]
pub struct RatholeClient {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    child: Option<Child>,
    config_path: Option<PathBuf>,
    last_config: Option<RatholeConfig>,
}

impl RatholeClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the rathole client with the given config.
    ///
    /// If the client is already running, it is stopped first — this is
    /// effectively a "reload with new config" operation.
    pub async fn start(&self, config: RatholeConfig) -> Result<(), TunnelError> {
        let mut inner = self.inner.lock().await;

        // Tear down any previous instance so we can't leak a child.
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill().await;
        }

        let bin = resolve_rathole_binary().ok_or(TunnelError::BinaryNotFound)?;
        let config_path = write_config_to_temp(&config)
            .await
            .map_err(|e| TunnelError::ConfigIo(e.to_string()))?;

        debug!(
            binary = %bin.display(),
            config = %config_path.display(),
            server = %config.server_addr,
            "spawning rathole client"
        );

        let child = crate::process_helpers::tokio_no_window(&bin)
            .arg("--client")
            .arg(&config_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| TunnelError::SpawnFailed(e.to_string()))?;

        info!(server = %config.server_addr, "rathole client started");

        inner.child = Some(child);
        inner.config_path = Some(config_path);
        inner.last_config = Some(config);
        Ok(())
    }

    /// Stop the rathole client. Safe to call when not running.
    pub async fn stop(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            if let Err(e) = child.kill().await {
                warn!(error = %e, "failed to kill rathole child");
            } else {
                info!("rathole client stopped");
            }
        }
        // Best-effort temp cleanup.
        if let Some(path) = inner.config_path.take() {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }

    /// True if the child process appears to still be running.
    ///
    /// Uses `try_wait` so a crashed child will flip this to `false` without us
    /// having to poll.
    pub async fn is_connected(&self) -> bool {
        let mut inner = self.inner.lock().await;
        let Some(child) = inner.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true, // still running
            Ok(Some(status)) => {
                warn!(?status, "rathole child exited");
                inner.child = None;
                false
            }
            Err(e) => {
                error!(error = %e, "try_wait on rathole child failed");
                false
            }
        }
    }

    /// Current config (if started).
    pub async fn current_config(&self) -> Option<RatholeConfig> {
        self.inner.lock().await.last_config.clone()
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// Locate the `rathole` binary. Returns `None` if nothing suitable is found.
pub fn resolve_rathole_binary() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("RATHOLE_BIN") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }

    let bin_name = if cfg!(windows) {
        "rathole.exe"
    } else {
        "rathole"
    };

    // Alongside the runner executable (bundled resource).
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let candidate = exe_dir.join(bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // Fallback to PATH.
    Some(PathBuf::from(bin_name))
}

async fn write_config_to_temp(config: &RatholeConfig) -> std::io::Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push("qontinui-rathole");
    tokio::fs::create_dir_all(&dir).await?;
    let file_name = format!("client-{}.toml", std::process::id());
    let path = dir.join(file_name);
    let mut file = tokio::fs::File::create(&path).await?;
    file.write_all(config.to_toml().as_bytes()).await?;
    file.flush().await?;
    Ok(path)
}

#[allow(dead_code)]
fn _path_unused(_: &Path) {}
