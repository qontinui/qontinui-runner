//! Wrapper subprocess manager: spawn / stop / health / idle reaper.
//!
//! ## Why this is a parallel manager (not an extension of ProcessCaptureManager)
//!
//! `process_capture::ProcessCaptureManager` is purpose-built for the
//! "named user-configured background service" workflow: it expects a
//! `ProcessConfig` registered ahead of time, surfaces output in the
//! Processes UI, ties into the build-error monitor, and exposes
//! per-process restart / rebuild verbs. Wrappers have a fundamentally
//! different lifecycle — lazy spawn keyed by a registry id, idle reaping,
//! per-spawn injected env from the credential store, no UI side, no
//! build pipeline — so wedging that lifecycle into the existing manager
//! would either pollute its config schema or require enough special-cases
//! to amount to a parallel implementation anyway. We build a focused
//! `WrapperManager` instead; the two managers can coexist behind their
//! respective HTTP routes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use super::credentials::CredentialStore;
use super::manifest::WrapperManifest;
use super::registry::WrapperRegistry;

/// Default per-spawn readiness wait. The plan calls for "wait for the WS
/// endpoint to become reachable" — for `transport: "api"` the wrapper
/// listens on a TCP port and we treat connect-success as ready.
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Polling cadence inside the readiness loop.
const SPAWN_READY_POLL: Duration = Duration::from_millis(150);

/// Health check cadence (plan §1.3).
pub const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// Max consecutive health failures before we mark the wrapper degraded.
pub const HEALTH_MAX_RETRIES: u32 = 3;
/// Idle reaper threshold (plan §1.3): no dispatch in this window → stop.
pub const IDLE_REAP_AFTER: Duration = Duration::from_secs(10 * 60);
/// Reaper sweep cadence.
const REAPER_INTERVAL: Duration = Duration::from_secs(60);
/// Graceful stop budget before we force-kill (plan §1.3).
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Lifecycle state of a managed wrapper subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WrapperState {
    /// No subprocess is running.
    Stopped,
    /// Spawned and verified reachable.
    Running,
    /// Health checks failing, retries exhausted.
    Degraded,
}

/// Snapshot of a wrapper's runtime state suitable for `GET /wrappers/:id/status`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WrapperStatus {
    pub id: String,
    pub state: WrapperState,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    /// Unix epoch milliseconds.
    pub started_at_ms: Option<i64>,
    pub last_dispatch_at_ms: Option<i64>,
    pub consecutive_health_failures: u32,
}

/// Internal runtime record. Held inside the manager's RwLock.
struct WrapperRuntime {
    id: String,
    port: u16,
    pid: Option<u32>,
    child: Option<Child>,
    started_at: Instant,
    last_dispatch: Instant,
    consecutive_failures: u32,
    state: WrapperState,
}

/// The wrapper manager.
pub struct WrapperManager {
    registry: Arc<WrapperRegistry>,
    /// Per-wrapper runtime records.
    runtimes: RwLock<HashMap<String, WrapperRuntime>>,
    /// Per-wrapper spawn lock — ensures `spawn(id)` is idempotent even
    /// under concurrent callers.
    spawn_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl WrapperManager {
    pub fn new(registry: Arc<WrapperRegistry>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            runtimes: RwLock::new(HashMap::new()),
            spawn_locks: Mutex::new(HashMap::new()),
        })
    }

    /// Lazy-spawn a wrapper subprocess. Idempotent: if the wrapper is
    /// already `Running`, returns the existing port without re-spawning.
    pub async fn spawn(&self, wrapper_id: &str) -> Result<u16, String> {
        // Per-wrapper lock so two concurrent dispatches don't race on
        // spawn.
        let lock = self.spawn_lock_for(wrapper_id).await;
        let _guard = lock.lock().await;

        // Fast path: already routable. `port_for` is the one Rust definition
        // of "routable" (Running only); the frontend mirrors it as
        // `isWrapperRoutable` in `src/lib/wrappers/status.ts`.
        if let Some(port) = self.port_for(wrapper_id).await {
            return Ok(port);
        }

        let wrapper = self
            .registry
            .get(wrapper_id)
            .await
            .ok_or_else(|| format!("wrapper '{}' is not installed", wrapper_id))?;

        // Phase 1 only enables `api`. Surface early so callers don't get
        // a confusing readiness timeout against a transport that never
        // binds a port.
        if wrapper.manifest.transport != "api" {
            return Err(format!(
                "wrapper '{}' declares transport '{}', but only 'api' is enabled in this runner",
                wrapper_id, wrapper.manifest.transport
            ));
        }

        let install_path = PathBuf::from(&wrapper.install_path);
        let entry = install_path.join("dist").join("index-node.js");
        if !entry.exists() {
            return Err(format!(
                "wrapper '{}' has no dist/index-node.js at {} — reinstall it",
                wrapper_id,
                entry.display()
            ));
        }

        // Re-check under ONE write lock: the health loop (which does not take
        // the spawn lock) may have flipped a Degraded record back to Running
        // since the fast path missed, in which case that record wins.
        if let ExistingRecord::Routable(port) = self.claim_or_retire(wrapper_id).await {
            return Ok(port);
        }

        let port = pick_free_port()
            .map_err(|e| format!("failed to allocate port for '{}': {}", wrapper_id, e))?;
        let env = build_env(wrapper_id, &wrapper.manifest);

        let mut cmd = crate::process_helpers::tokio_no_window("node");
        cmd.current_dir(&install_path)
            .arg(&entry)
            .env("WRAPPER_PORT", port.to_string())
            .env("PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &env {
            cmd.env(k, v);
        }
        // Inherit only the bare minimum from the parent process — not
        // strictly required (Node will pick up PATH etc. from the
        // inherited env by default) but we *do not* override env_clear
        // because Node needs PATH/HOMEDRIVE/HOMEPATH on Windows.

        let child = cmd.spawn().map_err(|e| {
            format!(
                "failed to spawn wrapper '{}' (node {}): {}",
                wrapper_id,
                entry.display(),
                e
            )
        })?;
        let pid = child.id();

        let mut runtime = WrapperRuntime {
            id: wrapper_id.to_string(),
            port,
            pid,
            child: Some(child),
            started_at: Instant::now(),
            last_dispatch: Instant::now(),
            consecutive_failures: 0,
            state: WrapperState::Running,
        };

        // Wait for the wrapper's port to become reachable before we
        // declare the spawn successful. If it never does, route the child
        // through the same terminate_runtime used by stop()/claim_or_retire
        // so it is waited on (and force-killed after STOP_GRACE) rather than
        // just signalled and dropped.
        if let Err(e) = wait_for_port(port, SPAWN_READY_TIMEOUT).await {
            terminate_runtime(wrapper_id, runtime).await;
            return Err(format!(
                "wrapper '{}' did not become ready on port {} within {:?}: {}",
                wrapper_id, port, SPAWN_READY_TIMEOUT, e
            ));
        }

        info!(
            "wrappers: spawned '{}' on port {} (pid {:?})",
            wrapper_id, port, pid
        );
        self.runtimes
            .write()
            .await
            .insert(wrapper_id.to_string(), runtime);
        Ok(port)
    }

    /// Settle an existing runtime record before `spawn` replaces it,
    /// atomically: under ONE write lock, a Running record is kept and its port
    /// returned (spawn must not start a second process), and any other record
    /// (Degraded) is REMOVED; its child is then killed and reaped outside the
    /// lock. A Degraded record still owns its subprocess — which is why the
    /// UI offers Stop for it — and letting `spawn`'s `insert` replace it would
    /// drop that `Child` handle without killing it, orphaning the process.
    /// A separate read-then-stop would race the health loop, which can flip
    /// Degraded -> Running between the two without holding the spawn lock.
    async fn claim_or_retire(&self, wrapper_id: &str) -> ExistingRecord {
        let removed = {
            let mut runtimes = self.runtimes.write().await;
            match runtimes.get(wrapper_id) {
                Some(rt) if rt.state == WrapperState::Running => {
                    return ExistingRecord::Routable(rt.port);
                }
                Some(_) => runtimes.remove(wrapper_id),
                None => return ExistingRecord::Absent,
            }
        };
        if let Some(runtime) = removed {
            terminate_runtime(wrapper_id, runtime).await;
        }
        ExistingRecord::Retired
    }

    /// Stop a running wrapper. SIGTERM first, then force-kill after
    /// `STOP_GRACE` if the child hasn't exited.
    pub async fn stop(&self, wrapper_id: &str) -> Result<(), String> {
        let removed = self.runtimes.write().await.remove(wrapper_id);
        if let Some(runtime) = removed {
            terminate_runtime(wrapper_id, runtime).await;
        }
        Ok(())
    }

    /// Status snapshot for one wrapper. Returns `Stopped` if the wrapper
    /// has no live runtime record.
    pub async fn status(&self, wrapper_id: &str) -> WrapperStatus {
        let runtimes = self.runtimes.read().await;
        match runtimes.get(wrapper_id) {
            Some(rt) => WrapperStatus {
                id: wrapper_id.to_string(),
                state: rt.state,
                port: Some(rt.port),
                pid: rt.pid,
                started_at_ms: Some(instant_to_epoch_ms(rt.started_at)),
                last_dispatch_at_ms: Some(instant_to_epoch_ms(rt.last_dispatch)),
                consecutive_health_failures: rt.consecutive_failures,
            },
            None => WrapperStatus {
                id: wrapper_id.to_string(),
                state: WrapperState::Stopped,
                port: None,
                pid: None,
                started_at_ms: None,
                last_dispatch_at_ms: None,
                consecutive_health_failures: 0,
            },
        }
    }

    /// Return the port dispatch may route to, if any: `Running` ONLY. A
    /// `Degraded` wrapper keeps its runtime record (and its subprocess, so it
    /// can still be stopped) but is not routable — `spawn` replaces it
    /// instead of reusing it. The frontend mirrors this split as
    /// `isWrapperRoutable` / `isWrapperProcessAlive`
    /// (`src/lib/wrappers/status.ts`).
    pub async fn port_for(&self, wrapper_id: &str) -> Option<u16> {
        self.runtimes
            .read()
            .await
            .get(wrapper_id)
            .filter(|rt| rt.state == WrapperState::Running)
            .map(|rt| rt.port)
    }

    /// Update the last-dispatch timestamp for the idle reaper. Called by
    /// `dispatch::dispatch_handler` after a successful proxy.
    pub async fn note_dispatch(&self, wrapper_id: &str) {
        if let Some(rt) = self.runtimes.write().await.get_mut(wrapper_id) {
            rt.last_dispatch = Instant::now();
        }
    }

    /// Apply one health-probe result to the runtime record it was taken
    /// for. `id`/`port` are a snapshot from before the (awaited) probe ran;
    /// `spawn` may have retired that record and inserted a new one (a fresh
    /// port) while the probe was in flight, via `claim_or_retire`. Checking
    /// `rt.port == port` refuses to charge a failed probe of the OLD port
    /// against a process that was never asked, and refuses to reset the
    /// NEW record's failure count on a stale success.
    async fn apply_health_probe(self: &Arc<Self>, id: &str, port: u16, healthy: bool) {
        let mut runtimes = self.runtimes.write().await;
        let rt = match runtimes.get_mut(id) {
            Some(r) => r,
            None => return,
        };
        if rt.port != port {
            return;
        }
        if healthy {
            rt.consecutive_failures = 0;
            if rt.state == WrapperState::Degraded {
                rt.state = WrapperState::Running;
            }
        } else {
            rt.consecutive_failures = rt.consecutive_failures.saturating_add(1);
            if rt.consecutive_failures >= HEALTH_MAX_RETRIES && rt.state != WrapperState::Degraded {
                warn!(
                    "wrappers: '{}' health check failed {}x — marking degraded",
                    id, rt.consecutive_failures
                );
                rt.state = WrapperState::Degraded;
                // Drop the lock before respawn; it acquires its own write
                // guard.
                drop(runtimes);
                let _ = self.restart_after_degraded(id).await;
            }
        }
    }

    /// Background task: probe each running wrapper's port. After
    /// `HEALTH_MAX_RETRIES` consecutive failures the wrapper is marked
    /// `Degraded` and a single restart attempt is made.
    pub fn spawn_health_loop(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEALTH_CHECK_INTERVAL).await;
                let ids: Vec<(String, u16)> = manager
                    .runtimes
                    .read()
                    .await
                    .iter()
                    .filter(|(_, rt)| rt.state != WrapperState::Stopped)
                    .map(|(id, rt)| (id.clone(), rt.port))
                    .collect();
                for (id, port) in ids {
                    let healthy = probe_port(port).await.is_ok();
                    manager.apply_health_probe(&id, port, healthy).await;
                }
            }
        });
    }

    /// Background task: stop wrappers that haven't dispatched in
    /// `IDLE_REAP_AFTER`.
    pub fn spawn_idle_reaper(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAPER_INTERVAL).await;
                let now = Instant::now();
                let candidates: Vec<String> = manager
                    .runtimes
                    .read()
                    .await
                    .iter()
                    .filter(|(_, rt)| {
                        rt.state == WrapperState::Running
                            && now.duration_since(rt.last_dispatch) >= IDLE_REAP_AFTER
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in candidates {
                    info!("wrappers: idle-reaping '{}'", id);
                    if let Err(e) = manager.stop(&id).await {
                        warn!("wrappers: idle-reap stop('{}') failed: {}", id, e);
                    }
                }
            }
        });
    }

    /// Stop all wrappers. Useful for shutdown / test teardown.
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.runtimes.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id).await;
        }
    }

    async fn restart_after_degraded(self: &Arc<Self>, wrapper_id: &str) -> Result<(), String> {
        // Tear down whatever's left.
        if let Err(e) = self.stop(wrapper_id).await {
            warn!(
                "wrappers: stop('{}') during restart_after_degraded failed: {}",
                wrapper_id, e
            );
        }
        // One re-spawn attempt; if it fails the entry stays gone and the
        // next dispatch will lazy-spawn fresh.
        match self.spawn(wrapper_id).await {
            Ok(port) => {
                info!(
                    "wrappers: '{}' recovered from degraded state on port {}",
                    wrapper_id, port
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    "wrappers: respawn('{}') after degraded failed: {}",
                    wrapper_id, e
                );
                Err(e)
            }
        }
    }

    async fn spawn_lock_for(&self, wrapper_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.spawn_locks.lock().await;
        locks
            .entry(wrapper_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Build the env vars to inject into the subprocess. Pulls every declared
/// `envVars[].name` from the credential store; missing-but-required vars
/// are *not* surfaced as a hard error here — that's the wrapper's call
/// (it can refuse to start, run a self-test, etc.). We log a warning for
/// visibility.
fn build_env(wrapper_id: &str, manifest: &WrapperManifest) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(manifest.env_vars.len());
    for spec in &manifest.env_vars {
        match CredentialStore::get(wrapper_id, &spec.name) {
            Ok(Some(value)) => out.push((spec.name.clone(), value)),
            Ok(None) if spec.required => {
                warn!(
                    "wrappers: required env var '{}' for wrapper '{}' is not set",
                    spec.name, wrapper_id
                );
            }
            Ok(None) => {
                debug!(
                    "wrappers: optional env var '{}' for wrapper '{}' not set",
                    spec.name, wrapper_id
                );
            }
            Err(e) => {
                warn!(
                    "wrappers: env var '{}' for wrapper '{}' read error: {}",
                    spec.name, wrapper_id, e
                );
            }
        }
    }
    out
}

/// Pick a free localhost TCP port by binding to port 0 and reading back
/// the assigned port number, then dropping the socket so the wrapper can
/// rebind it.
fn pick_free_port() -> Result<u16, std::io::Error> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

/// Block until a TCP connect to `127.0.0.1:port` succeeds or the
/// timeout elapses.
async fn wait_for_port(port: u16, total: Duration) -> Result<(), String> {
    let deadline = Instant::now() + total;
    let mut last_err = "no attempts".to_string();
    while Instant::now() < deadline {
        match probe_port(port).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
        tokio::time::sleep(SPAWN_READY_POLL).await;
    }
    Err(last_err)
}

async fn probe_port(port: u16) -> Result<(), String> {
    match tokio::time::timeout(
        Duration::from_millis(500),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("connect: {}", e)),
        Err(_) => Err("connect: timed out".to_string()),
    }
}

fn instant_to_epoch_ms(then: Instant) -> i64 {
    // Approximate — Instant is monotonic and not anchored to the wall
    // clock, but we just need a "close to now" timestamp for status UI.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let elapsed_ms = then.elapsed().as_millis() as i64;
    now_ms - elapsed_ms
}

/// Convenience: confirm `node` is on the PATH. Used by the install
/// pipeline at startup so we can fail fast with a useful error rather
/// than letting every spawn explode mysteriously.
pub fn node_available() -> bool {
    crate::process_helpers::no_window("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What `claim_or_retire` found for a wrapper about to be (re)spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingRecord {
    /// A Running record exists; `spawn` returns this port.
    Routable(u16),
    /// A non-routable (Degraded) record existed and was removed + terminated.
    Retired,
    /// No record — nothing to settle.
    Absent,
}

/// Kill and reap a runtime record's child that has ALREADY been removed from
/// the map (so no lock is held while waiting). SIGTERM-equivalent first, then
/// force-kill after `STOP_GRACE` if the child hasn't exited.
async fn terminate_runtime(wrapper_id: &str, mut runtime: WrapperRuntime) {
    let mut child = match runtime.child.take() {
        Some(c) => c,
        None => return,
    };

    // tokio::process::Child has `start_kill` which on Windows uses
    // TerminateProcess and on unix sends SIGKILL. There's no portable
    // SIGTERM equivalent in tokio; on Windows the runner's other
    // managers (ProcessCaptureManager) also use TerminateProcess, so
    // we follow that precedent. If a wrapper needs graceful shutdown
    // semantics it should declare its own /shutdown HTTP path which
    // higher-level callers can invoke before `stop`.
    if let Err(e) = child.start_kill() {
        warn!(
            "wrappers: start_kill('{}') failed: {} — proceeding to wait",
            wrapper_id, e
        );
    }

    // Wait up to STOP_GRACE for the child to exit.
    match tokio::time::timeout(STOP_GRACE, child.wait()).await {
        Ok(Ok(status)) => {
            debug!("wrappers: '{}' exited with {}", wrapper_id, status);
        }
        Ok(Err(e)) => warn!("wrappers: wait('{}') failed: {}", wrapper_id, e),
        Err(_) => {
            warn!(
                "wrappers: '{}' did not exit within {:?}, killing",
                wrapper_id, STOP_GRACE
            );
            let _ = child.kill().await;
        }
    }
}

/// Provided so the bare `Path` import is exercised; future install code
/// will resolve `<root>/<id>` via this.
#[allow(dead_code)]
pub fn install_path_for(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Insert a runtime record in `state` with no child process — the
    /// shape a wrapper has after the health loop flips it (the process is
    /// still owned by the record; only its health verdict changed).
    async fn manager_with(state: WrapperState) -> (Arc<WrapperManager>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let registry = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        let manager = WrapperManager::new(registry);
        let now = Instant::now();
        manager.runtimes.write().await.insert(
            "w".to_string(),
            WrapperRuntime {
                id: "w".to_string(),
                port: 41234,
                pid: Some(7),
                child: None,
                started_at: now,
                last_dispatch: now,
                consecutive_failures: HEALTH_MAX_RETRIES,
                state,
            },
        );
        (manager, tmp)
    }

    /// The contract the frontend's `isWrapperRoutable` /
    /// `isWrapperProcessAlive` (`src/lib/wrappers/status.ts`) mirror: a
    /// `Degraded` wrapper still has a live runtime record (so `stop` has
    /// something to stop) but `port_for` refuses to route to it.
    #[tokio::test]
    async fn degraded_is_alive_but_not_routable() {
        let (manager, _tmp) = manager_with(WrapperState::Degraded).await;
        assert_eq!(manager.port_for("w").await, None);
        let status = manager.status("w").await;
        assert_eq!(status.state, WrapperState::Degraded);
        assert_eq!(status.port, Some(41234));
        manager.stop("w").await.unwrap();
        assert_eq!(manager.status("w").await.state, WrapperState::Stopped);
    }

    /// `spawn` retires a Degraded record before replacing it, killing the
    /// subprocess it still owns instead of dropping (orphaning) the handle.
    /// Uses a real `sleep` child so the kill is observed, hence unix-only;
    /// the decision half is covered cross-platform below.
    #[cfg(unix)]
    #[tokio::test]
    async fn claim_or_retire_kills_the_degraded_child() {
        let (manager, _tmp) = manager_with(WrapperState::Degraded).await;
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id().unwrap();
        manager.runtimes.write().await.get_mut("w").unwrap().child = Some(child);

        assert_eq!(manager.claim_or_retire("w").await, ExistingRecord::Retired);
        assert_eq!(manager.status("w").await.state, WrapperState::Stopped);
        // The process is gone (reaped by `terminate_runtime`): signal 0 fails.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "degraded child {pid} survived claim_or_retire");
    }

    /// Decision half: a Running record is returned untouched (spawn must not
    /// start a second process — the health-loop race case), a Degraded one is
    /// retired, and an absent one is a no-op.
    #[tokio::test]
    async fn claim_or_retire_keeps_running_retires_degraded_ignores_absent() {
        let (manager, _tmp) = manager_with(WrapperState::Running).await;
        assert_eq!(
            manager.claim_or_retire("w").await,
            ExistingRecord::Routable(41234)
        );
        assert_eq!(manager.port_for("w").await, Some(41234));
        assert_eq!(manager.status("w").await.state, WrapperState::Running);

        assert_eq!(
            manager.claim_or_retire("absent").await,
            ExistingRecord::Absent
        );

        let (degraded, _tmp2) = manager_with(WrapperState::Degraded).await;
        assert_eq!(degraded.claim_or_retire("w").await, ExistingRecord::Retired);
        assert_eq!(degraded.status("w").await.state, WrapperState::Stopped);
    }

    /// Negative control: the same record in `Running` IS routable.
    #[tokio::test]
    async fn running_is_routable() {
        let (manager, _tmp) = manager_with(WrapperState::Running).await;
        assert_eq!(manager.port_for("w").await, Some(41234));
    }

    /// Pin the `GET /wrappers/:id/status` wire shape the TS
    /// `WrapperStatusInfo` mirrors: the field is `state` (NOT `status`), the
    /// values are lowercase, and absent options serialize as `null`.
    #[tokio::test]
    async fn status_wire_shape_matches_the_ts_mirror() {
        let (manager, _tmp) = manager_with(WrapperState::Degraded).await;
        let json = serde_json::to_value(manager.status("w").await).unwrap();
        assert_eq!(json["state"], "degraded");
        assert!(json.get("status").is_none());
        assert_eq!(json["port"], 41234);
        assert_eq!(json["consecutive_health_failures"], HEALTH_MAX_RETRIES);
        let stopped = serde_json::to_value(manager.status("absent").await).unwrap();
        assert_eq!(stopped["state"], "stopped");
        assert!(stopped["port"].is_null());
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "consecutive_health_failures",
                "id",
                "last_dispatch_at_ms",
                "pid",
                "port",
                "started_at_ms",
                "state",
            ]
        );
    }

    /// A probe result whose `port` no longer matches the record's CURRENT
    /// port (as if `spawn` had retired and replaced the record while the
    /// probe was in flight) must be dropped rather than applied — the
    /// pre-fix behaviour charged a failed probe of the OLD port to the NEW
    /// record. `manager_with` seeds port 41234 and `consecutive_failures =
    /// HEALTH_MAX_RETRIES`; probing a different port simulates the stale
    /// snapshot and must leave both untouched.
    #[tokio::test]
    async fn stale_probe_port_is_ignored() {
        let (manager, _tmp) = manager_with(WrapperState::Running).await;
        manager.apply_health_probe("w", 9999, false).await;
        let status = manager.status("w").await;
        assert_eq!(status.consecutive_health_failures, HEALTH_MAX_RETRIES);
        assert_eq!(status.state, WrapperState::Running);
    }

    /// Negative control: a probe result for the record's CURRENT port is
    /// applied normally. Reset the seeded failure count to 0 first so the
    /// failure path doesn't also cross HEALTH_MAX_RETRIES and trigger a
    /// degrade/restart, which is a different behaviour covered elsewhere.
    #[tokio::test]
    async fn current_port_probe_is_applied() {
        let (manager, _tmp) = manager_with(WrapperState::Running).await;
        manager
            .runtimes
            .write()
            .await
            .get_mut("w")
            .unwrap()
            .consecutive_failures = 0;
        manager.apply_health_probe("w", 41234, false).await;
        assert_eq!(manager.status("w").await.consecutive_health_failures, 1);
        manager.apply_health_probe("w", 41234, true).await;
        assert_eq!(manager.status("w").await.consecutive_health_failures, 0);
    }
}
