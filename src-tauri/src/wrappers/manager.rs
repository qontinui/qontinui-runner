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

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::process_helpers::ChildTreeGuard;

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
/// Readiness wait for a browser transport (`headless` / `headed`): `node`
/// plus a Chromium launch. Measured 2026-10-17 on a Linux box under load
/// average ~32: a headless transport reached `ready()` in 389-402 ms warm;
/// a headed launch on a cold Windows desktop is slower and unmeasured, so the
/// browser kinds get twice `api`'s budget rather than the same.
const BROWSER_SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a bounded spawn did not produce a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// Another spawn of this wrapper held its lock past the caller's wait:
    /// nothing was started.
    Busy,
    /// The spawn ran and failed.
    Failed(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Busy => write!(f, "another spawn of this wrapper is in progress"),
            SpawnError::Failed(e) => f.write_str(e),
        }
    }
}

/// An upper bound on what a dispatch spends BEFORE the wrapper receives it.
/// Its named parts are each bounded: the waits for an exclusive turn and the
/// spawn lock (dispatch.rs `PRE_DISPATCH_WAIT`, 30 s together), retiring a
/// Degraded record (`terminate_runtime`: up to three `STOP_GRACE` waits), a
/// browser wrapper's readiness wait (30 s) and terminating a spawn that never
/// became ready (three more). Around them sit small costs that are not capped
/// one by one — a readiness poll's overshoot, the 100 ms output settle, the
/// `/proc` scans — so the bound carries a 10 s margin rather than claiming to
/// be exact. A caller sizing its read timeout adds its dispatch timeout to it.
pub const PRE_DISPATCH_CEILING: Duration = Duration::from_secs(30 + 15 + 30 + 15 + 10);

/// The readiness wait for a wrapper declaring `transport`.
fn ready_timeout_for(transport: &str) -> Duration {
    match transport {
        "headless" | "headed" => BROWSER_SPAWN_READY_TIMEOUT,
        _ => SPAWN_READY_TIMEOUT,
    }
}

/// Lines of wrapper output kept for a spawn-failure error.
const OUTPUT_TAIL_LINES: usize = 40;
/// Longest single kept line, and the most the error quotes in total.
const OUTPUT_LINE_MAX: usize = 400;
const OUTPUT_ERROR_MAX: usize = 2000;

/// A bounded ring of the wrapper's recent output lines.
#[derive(Clone, Default)]
struct OutputTail(Arc<std::sync::Mutex<VecDeque<String>>>);

impl OutputTail {
    /// Keep what `classify_line` allows of a line the WRAPPER printed.
    fn push(&self, line: &str) {
        let line = classify_line(line);
        if !line.is_empty() {
            self.push_own(line);
        }
    }

    /// Keep a marker the RUNNER wrote itself (already safe, never classified).
    fn push_own(&self, mut line: String) {
        if line.len() > OUTPUT_LINE_MAX {
            let mut cut = OUTPUT_LINE_MAX;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push('…');
        }
        if let Ok(mut q) = self.0.lock() {
            if q.len() == OUTPUT_TAIL_LINES {
                q.pop_front();
            }
            q.push_back(line);
        }
    }

    /// The newest lines, oldest first, capped at `OUTPUT_ERROR_MAX` bytes.
    fn render(&self) -> String {
        let q = match self.0.lock() {
            Ok(q) => q.clone(),
            Err(_) => return String::new(),
        };
        let mut out: Vec<String> = Vec::new();
        let mut total = 0;
        for line in q.iter().rev() {
            total += line.len() + 1;
            if total > OUTPUT_ERROR_MAX {
                break;
            }
            out.push(line.clone());
        }
        out.reverse();
        out.join("\n")
    }

    /// Drain `stream` into the ring until it closes. Reads BYTES, not lines:
    /// a non-UTF-8 byte is decoded lossily instead of ending the drain (which
    /// would close the pipe and hand the wrapper an EPIPE), and a line is never
    /// buffered past `OUTPUT_LINE_MAX` bytes (the rest of a long line — a base64
    /// dump — is read and discarded up to its newline).
    fn drain<R: AsyncRead + Unpin + Send + 'static>(&self, wrapper_id: &str, stream: R) {
        let tail = self.clone();
        let id = wrapper_id.to_string();
        tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let mut line: Vec<u8> = Vec::with_capacity(OUTPUT_LINE_MAX);
            let mut overflow = false;
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    // A read error on a pipe is the end of it, never a reason
                    // to stop reading earlier than that.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                for &b in buf.iter().take(n) {
                    if b == b'\n' {
                        let text = String::from_utf8_lossy(&line);
                        debug!("wrapper[{}]: {}", id, classify_line(&text));
                        tail.push(&text);
                        if overflow {
                            tail.push_own("[line cut]".to_string());
                        }
                        line.clear();
                        overflow = false;
                    } else if line.len() < OUTPUT_LINE_MAX {
                        line.push(b);
                    } else {
                        overflow = true;
                    }
                }
            }
            if !line.is_empty() {
                tail.push(&String::from_utf8_lossy(&line));
            }
        });
    }
}

/// Header names whose whole line is dropped from quoted wrapper output.
/// The one free-form-looking line a wrapper may print that is kept verbatim:
/// the structured diagnostic the private wrappers emit. Every field is an
/// enum-like word or a short identifier — no free text, no spaces in a value.
static WRAPPER_DIAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"^\[wrapper\] event=[a-z_]{1,32} code=[a-z_]{1,32}(?: detail=[A-Za-z0-9_:.-]{1,64})?$",
    )
    .expect("static regex")
});
/// Codes worth keeping from any other line, each EXTRACTED (never the line):
/// a Chromium network error, a Node error code, an error class name, an exit
/// status, and Playwright's missing-browser message reduced to a code.
static NET_ERR: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"net::ERR_[A-Z_]{1,64}").expect("static regex"));
static NODE_ERR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?:^|[^:A-Za-z0-9_])(ERR_[A-Z_]{1,64})\b").expect("static regex")
});
static ERROR_CLASS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b((?:[A-Z][A-Za-z]{0,40})?Error)\b").expect("static regex")
});
static EXIT_STATUS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\bexit(?:ed with)? (?:code|status) (-?\d{1,4})\b")
        .expect("static regex")
});
static EXECUTABLE_MISSING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)executable doesn't exist").expect("static regex")
});

/// A line's length and a sha256 prefix: the evidence that something was
/// printed, without its content.
pub(crate) fn omitted(line: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(line.as_bytes());
    format!(
        "omitted(len={}, sha256={})",
        line.len(),
        hex::encode(&digest[..6])
    )
}

/// What may be kept of one line a wrapper printed. NOTHING is kept verbatim
/// except a well-formed `[wrapper] event=… code=… [detail=…]` diagnostic;
/// from any other line only the allowlisted codes are extracted, and a line
/// holding none of them becomes `omitted(len, sha256)`. An allowlist, not a
/// redaction: a cookie split across Node's multi-line `inspect` output, an
/// escaped JSON body or a header shape nobody anticipated never reaches a log,
/// an error tail or an HTTP body, because no pattern has to recognise it.
fn classify_line(line: &str) -> String {
    let t = line.trim();
    if t.is_empty() {
        return String::new();
    }
    if WRAPPER_DIAG.is_match(t) {
        return t.to_string();
    }
    let mut kept: Vec<String> = Vec::new();
    let mut push = |v: String| {
        if !kept.contains(&v) {
            kept.push(v);
        }
    };
    if EXECUTABLE_MISSING.is_match(t) {
        push("playwright:executable_missing".to_string());
    }
    for m in ERROR_CLASS.captures_iter(t) {
        push(m[1].to_string());
    }
    for m in NET_ERR.find_iter(t) {
        push(m.as_str().to_string());
    }
    for m in NODE_ERR.captures_iter(t) {
        push(m[1].to_string());
    }
    for m in EXIT_STATUS.captures_iter(t) {
        push(format!("exit_status={}", &m[1]));
    }
    if kept.is_empty() {
        omitted(t)
    } else {
        format!("{} ({})", kept.join(" "), omitted(t))
    }
}

/// Transports a runner-spawned wrapper may declare.
///
/// `@qontinui/ui-bridge-wrapper`'s `runWrapperEntrypoint` awaits
/// `transport.ready()` and then binds the `/dispatch` + `/health` server on
/// `WRAPPER_PORT` for EVERY transport kind, so `headless` and `headed`
/// (Playwright Chromium driven from the wrapper process) bind the port exactly
/// as `api` does — once the browser has launched, inside
/// `BROWSER_SPAWN_READY_TIMEOUT`. Phase 1 enabled `api` alone; `headless`/`headed` are
/// enabled for the private web-account image wrappers (plan
/// `2026-10-17-free-image-generation-local-gpu-and-private-ui-bridge-wrappers`).
/// `injected` (needs a target page carrying the IIFE) and `live` (connects back
/// to this runner's WebSocket) stay refused until a consumer needs them.
pub const SPAWNABLE_TRANSPORTS: &[&str] = &["api", "headless", "headed"];

/// Whether [`WrapperManager::spawn`] will start a wrapper declaring `transport`.
pub fn transport_is_spawnable(transport: &str) -> bool {
    SPAWNABLE_TRANSPORTS.contains(&transport)
}

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
    /// The wrapper's whole process tree: its own process group (Unix) or a
    /// kill-on-close Job Object (Windows). A browser transport's Chromium
    /// processes are GRANDCHILDREN of `node`; killing `node` alone would
    /// orphan them. Dropping the guard kills the tree.
    tree: Option<ChildTreeGuard>,
    /// The last lines the wrapper wrote to stdout/stderr (drained so a chatty
    /// wrapper can never block on a full pipe), kept for spawn-failure errors.
    output: OutputTail,
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
        self.spawn_within(wrapper_id, None)
            .await
            .map_err(|e| e.to_string())
    }

    /// [`Self::spawn`] whose wait for the per-wrapper spawn lock is bounded by
    /// `lock_wait`: past it the answer is [`SpawnError::Busy`] — nothing was
    /// started. Once the lock is held every remaining step is bounded by a
    /// constant (see [`PRE_DISPATCH_CEILING`]).
    pub async fn spawn_within(
        &self,
        wrapper_id: &str,
        lock_wait: Option<Duration>,
    ) -> Result<u16, SpawnError> {
        // Per-wrapper lock so two concurrent dispatches don't race on spawn.
        let lock = self.spawn_lock_for(wrapper_id).await;
        let _guard = match lock_wait {
            Some(wait) => tokio::time::timeout(wait, lock.lock_owned())
                .await
                .map_err(|_| SpawnError::Busy)?,
            None => lock.lock_owned().await,
        };
        self.spawn_locked(wrapper_id)
            .await
            .map_err(SpawnError::Failed)
    }

    /// The spawn itself; the caller holds this wrapper's spawn lock.
    async fn spawn_locked(&self, wrapper_id: &str) -> Result<u16, String> {
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

        // Refuse early a transport whose wrapper never binds `WRAPPER_PORT`,
        // so callers don't get a confusing readiness timeout.
        if !transport_is_spawnable(&wrapper.manifest.transport) {
            return Err(format!(
                "wrapper '{}' declares transport '{}', but only {:?} are enabled in this runner",
                wrapper_id, wrapper.manifest.transport, SPAWNABLE_TRANSPORTS
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
        // Own process group (Unix) before the exec, so the whole tree —
        // node AND any browser it launches — dies together on stop.
        ChildTreeGuard::arm_tokio(&mut cmd);
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

        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "failed to spawn wrapper '{}' (node {}): {}",
                wrapper_id,
                entry.display(),
                e
            )
        })?;
        let pid = child.id();
        let tree = ChildTreeGuard::attach_armed_tokio(&child);
        let output = OutputTail::default();
        if let Some(out) = child.stdout.take() {
            output.drain(wrapper_id, out);
        }
        if let Some(err) = child.stderr.take() {
            output.drain(wrapper_id, err);
        }

        let mut runtime = WrapperRuntime {
            id: wrapper_id.to_string(),
            port,
            pid,
            child: Some(child),
            tree: Some(tree),
            output: output.clone(),
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
        let ready_timeout = ready_timeout_for(&wrapper.manifest.transport);
        if let Err(e) = wait_for_port(port, ready_timeout, runtime.child.as_mut()).await {
            terminate_runtime(wrapper_id, runtime).await;
            // Give the drain tasks a moment to take the last lines a dying
            // process wrote before it closed its pipes.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let tail = output.render();
            return Err(format!(
                "wrapper '{}' did not become ready on port {} within {:?}: {}{}",
                wrapper_id,
                port,
                ready_timeout,
                e,
                if tail.is_empty() {
                    " (it wrote no output)".to_string()
                } else {
                    format!("; its last output:\n{}", tail)
                }
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

    pub(crate) async fn spawn_lock_for(&self, wrapper_id: &str) -> Arc<Mutex<()>> {
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

/// Block until a TCP connect to `127.0.0.1:port` succeeds, the timeout
/// elapses, or — when `child` is given — the wrapper process exits first (a
/// missing peer dependency or no display makes `node` exit at once; waiting
/// out the whole timeout would only hide that).
async fn wait_for_port(
    port: u16,
    total: Duration,
    mut child: Option<&mut Child>,
) -> Result<(), String> {
    let deadline = Instant::now() + total;
    let mut last_err = "no attempts".to_string();
    while Instant::now() < deadline {
        if let Some(c) = child.as_deref_mut() {
            if let Some(how) = exited_without_reaping(c) {
                return Err(format!("the wrapper process exited first ({how})"));
            }
        }
        match probe_port(port).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
        tokio::time::sleep(SPAWN_READY_POLL).await;
    }
    Err(last_err)
}

/// How `child` exited (`exit status N` / `signal N`), or `None` while it
/// runs — read WITHOUT reaping it on Unix (`WNOWAIT`), so its zombie keeps the
/// process-group id reserved for `terminate_runtime`'s group kill. Elsewhere
/// `try_wait` (a Windows job needs no pgid).
fn exited_without_reaping(child: &mut Child) -> Option<String> {
    #[cfg(unix)]
    {
        let Some(pid) = child.id() else {
            return Some("already reaped".to_string());
        };
        // SAFETY: waitid with WNOHANG|WNOWAIT only inspects our own child.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let rc = libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            );
            if rc != 0 || info.si_pid() == 0 {
                return None;
            }
            Some(if info.si_code == libc::CLD_EXITED {
                format!("exit status {}", info.si_status())
            } else {
                format!("signal {}", info.si_status())
            })
        }
    }
    #[cfg(not(unix))]
    {
        match child.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            _ => None,
        }
    }
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
    let tree = runtime.tree.take();
    let mut child = match runtime.child.take() {
        Some(c) => c,
        None => {
            drop(tree);
            return;
        }
    };

    // Playwright launches Chromium DETACHED on Unix (its own session and
    // process group), so a SIGKILL to the wrapper's group never reaches it,
    // and a SIGKILLed `node` cannot run the exit handlers that would have
    // closed it. So, in order:
    //   1. snapshot every descendant now, each with its kernel start time
    //      (once `node` dies they reparent to init and can no longer be found
    //      by ancestry; the start time later proves a pid was not reused);
    //   2. SIGTERM the group, so `runWrapperEntrypoint`'s handler closes the
    //      transport and Playwright closes its browser;
    //   3. wait up to STOP_GRACE for the leader to exit WITHOUT reaping it, so
    //      its zombie keeps the pgid reserved;
    //   4. SIGKILL the group while that is still true, then reap the leader;
    //   5. SIGKILL every snapshotted descendant whose start time still matches.
    // On Windows the kill-on-close Job Object holds every descendant whatever
    // its console or group, so dropping the guard is enough.
    #[cfg(unix)]
    {
        let leader = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .filter(|p| *p > 1);
        let descendants = leader.map(unix_descendants).unwrap_or_default();
        if let (true, Some(pgid)) = (tree.is_some(), leader) {
            // SAFETY: signal delivery to a group this manager created
            // (`process_group(0)` at spawn: pid == pgid); the leader has not
            // been reaped, so the pgid cannot have been reused.
            unsafe { libc::killpg(pgid, libc::SIGTERM) };
            let exited = wait_exit_without_reaping(pgid, STOP_GRACE).await;
            if !exited {
                warn!(
                    "wrappers: '{}' did not exit within {:?}, killing",
                    wrapper_id, STOP_GRACE
                );
            }
            // The leader is alive or a zombie: either way its pgid is still
            // reserved, so this group kill reaches only this wrapper's group.
            drop(tree);
        } else {
            if let Err(e) = child.start_kill() {
                warn!(
                    "wrappers: start_kill('{}') failed: {} — proceeding to wait",
                    wrapper_id, e
                );
            }
            match (tree, leader) {
                // The leader is already reaped: its pgid may belong to an
                // unrelated group by now, so the guard must not signal it.
                (Some(t), None) => t.disarm(),
                (t, _) => drop(t),
            }
        }
        reap(wrapper_id, &mut child).await;
        for d in descendants {
            d.kill_if_same();
        }
    }
    #[cfg(not(unix))]
    {
        // tokio's start_kill is TerminateProcess here; dropping the guard
        // closes the kill-on-close job and takes every descendant with it.
        if let Err(e) = child.start_kill() {
            warn!(
                "wrappers: start_kill('{}') failed: {} — proceeding to wait",
                wrapper_id, e
            );
        }
        drop(tree);
        reap(wrapper_id, &mut child).await;
    }
}

/// Reap the wrapper's `node`, force-killing it if it is still running after
/// STOP_GRACE.
async fn reap(wrapper_id: &str, child: &mut Child) {
    match tokio::time::timeout(STOP_GRACE, child.wait()).await {
        Ok(Ok(status)) => debug!("wrappers: '{}' exited with {}", wrapper_id, status),
        Ok(Err(e)) => warn!("wrappers: wait('{}') failed: {}", wrapper_id, e),
        Err(_) => {
            // Bounded too: a kill whose wait never returns must not hold the
            // caller (and its pre-dispatch budget) open.
            let _ = tokio::time::timeout(STOP_GRACE, child.kill()).await;
        }
    }
}

/// Wait up to `grace` for `pid` to exit, WITHOUT reaping it (`WNOWAIT`), so a
/// zombie keeps its pid — and so its process-group id — reserved. True when
/// it exited.
#[cfg(unix)]
async fn wait_exit_without_reaping(pid: i32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        // SAFETY: waitid with WNOHANG|WNOWAIT only inspects our own child.
        let exited = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let rc = libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            );
            rc == 0 && info.si_pid() != 0
        };
        if exited {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A descendant snapshotted with its kernel start time (`/proc/<pid>/stat`
/// field 22), so a later kill can prove the pid still names the same process.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Descendant {
    pid: i32,
    start_time: Option<u64>,
}

#[cfg(unix)]
impl Descendant {
    /// SIGKILL it only if its pid still names the SAME process: the start time
    /// read now equals the snapshot's. A pid whose start time cannot be read
    /// (gone, or no `/proc`) is left alone.
    fn kill_if_same(self) {
        let Some(then) = self.start_time else {
            return;
        };
        if proc_start_time(self.pid) == Some(then) {
            // SAFETY: SIGKILL to a pid proven to be the snapshotted process.
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}

/// `/proc/<pid>/stat` field 22 (starttime, in clock ticks since boot).
#[cfg(unix)]
fn proc_start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&stat).map(|(_, start)| start)
}

/// Every descendant of `root` (children, grandchildren, …), read from
/// `/proc`'s parent links, each with its start time. Linux only; elsewhere the
/// group kill and the SIGTERM phase are the whole story.
#[cfg(unix)]
fn unix_descendants(root: i32) -> Vec<Descendant> {
    #[cfg(target_os = "linux")]
    {
        // ONE read of each stat file gives both the parent link and the
        // start time, so the snapshot cannot pair a pid with a successor's
        // start time.
        let mut children: HashMap<i32, Vec<Descendant>> = HashMap::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<i32>().ok())
                else {
                    continue;
                };
                let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                if let Some((ppid, start_time)) = parse_stat(&stat) {
                    children.entry(ppid).or_default().push(Descendant {
                        pid,
                        start_time: Some(start_time),
                    });
                }
            }
        }
        let mut out = Vec::new();
        let mut queue = vec![root];
        while let Some(p) = queue.pop() {
            if let Some(kids) = children.get(&p) {
                for &k in kids {
                    out.push(k);
                    queue.push(k.pid);
                }
            }
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Vec::new()
    }
}

/// `(ppid, starttime)` from one `/proc/<pid>/stat` body: fields 4 and 22.
/// `comm` may hold spaces and parens, so fields are counted after the LAST
/// ')' — field 3 (state) is the first there, so 4 is index 1 and 22 index 19.
#[cfg(unix)]
fn parse_stat(stat: &str) -> Option<(i32, u64)> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let ppid = fields.nth(1)?.parse().ok()?;
    let start = fields.nth(17)?.parse().ok()?;
    Some((ppid, start))
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

    /// ONE number on both sides: qontinui-studio's
    /// `adapters/ui_bridge_wrapper.py` `PRE_DISPATCH_ALLOWANCE_S` mirrors this
    /// ceiling (100 s). Change both together.
    #[test]
    fn the_pre_dispatch_ceiling_is_the_documented_100s() {
        assert_eq!(PRE_DISPATCH_CEILING, Duration::from_secs(100));
    }

    /// `reap` force-kills a child that ignores the graceful wait, and returns
    /// within its bound (the grace plus the bounded kill), never hanging.
    #[cfg(unix)]
    #[tokio::test]
    async fn reap_force_kills_within_its_bound() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .spawn()
            .unwrap();
        let started = Instant::now();
        reap("w", &mut child).await;
        assert!(started.elapsed() < STOP_GRACE * 2 + Duration::from_secs(1));
        // It is gone: the kill landed.
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn browser_transports_get_the_longer_readiness_wait() {
        assert_eq!(ready_timeout_for("api"), SPAWN_READY_TIMEOUT);
        assert_eq!(ready_timeout_for("headless"), BROWSER_SPAWN_READY_TIMEOUT);
        assert_eq!(ready_timeout_for("headed"), BROWSER_SPAWN_READY_TIMEOUT);
        assert!(BROWSER_SPAWN_READY_TIMEOUT > SPAWN_READY_TIMEOUT);
    }

    #[test]
    fn output_tail_is_bounded() {
        let tail = OutputTail::default();
        for i in 0..(OUTPUT_TAIL_LINES + 10) {
            tail.push(&format!("[wrapper] event=line code=n detail=n{i}"));
        }
        let rendered = tail.render();
        assert!(
            !rendered.contains("detail=n0\n"),
            "the oldest lines are dropped"
        );
        assert!(rendered.ends_with(&format!("detail=n{}", OUTPUT_TAIL_LINES + 9)));
        assert_eq!(rendered.lines().count(), OUTPUT_TAIL_LINES);

        let long = OutputTail::default();
        for _ in 0..20 {
            long.push(&"x".repeat(5000));
        }
        assert!(long.render().len() <= OUTPUT_ERROR_MAX + OUTPUT_TAIL_LINES * 4);
    }

    /// Writes a fake wrapper into `root/w`: `--manifest-only` prints a
    /// manifest declaring `transport`, and a normal run executes `body`.
    #[cfg(unix)]
    fn fake_wrapper(root: &Path, transport: &str, body: &str) {
        let dir = root.join("w");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"w","version":"0.0.0"}"#,
        )
        .unwrap();
        let manifest = format!(
            r#"{{"manifestVersion":1,"manifest":{{"id":"w","displayName":"w","transport":"{transport}"}},"actions":[]}}"#
        );
        std::fs::write(
            dir.join("dist").join("index-node.js"),
            format!(
                "if (process.argv.includes('--manifest-only')) {{ console.log({manifest:?}); process.exit(0); }}\n{body}\n"
            ),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A browser transport's Chromium is a GRANDCHILD of the wrapper's `node`.
    /// `stop` must kill the whole tree, not only `node`: here the wrapper
    /// starts a `sleep` grandchild (the stand-in for Chromium) and serves its
    /// port, and after `stop` the grandchild must be gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_the_wrappers_grandchildren() {
        if !node_available() {
            eprintln!("skipped: node is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("grandchild.pid");
        let body = format!(
            "const g = require('child_process').spawn('sleep', ['300'], {{ stdio: 'ignore' }});\n\
             require('fs').writeFileSync({pid:?}, String(g.pid));\n\
             require('http').createServer((q, r) => r.end('ok')).listen(Number(process.env.WRAPPER_PORT), '127.0.0.1');",
            pid = pid_file.to_string_lossy()
        );
        fake_wrapper(tmp.path(), "headless", &body);
        let registry = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        let manager = WrapperManager::new(registry);

        manager.spawn("w").await.expect("the fake wrapper spawns");
        let grandchild: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(grandchild), "precondition: the grandchild runs");

        manager.stop("w").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(grandchild) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive(grandchild), "grandchild {grandchild} survived stop");
    }

    /// Playwright launches Chromium DETACHED (its own session and group), so
    /// a group kill misses it. Here the wrapper starts a `setsid`-style
    /// detached grandchild and ignores SIGTERM's chance to clean up; the
    /// recorded-descendant belt must still kill it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stop_kills_a_detached_grandchild_outside_the_group() {
        if !node_available() {
            eprintln!("skipped: node is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("detached.pid");
        let body = format!(
            "const g = require('child_process').spawn('sleep', ['300'], {{ detached: true, stdio: 'ignore' }});\n\
             g.unref();\n\
             require('fs').writeFileSync({pid:?}, String(g.pid));\n\
             require('http').createServer((q, r) => r.end('ok')).listen(Number(process.env.WRAPPER_PORT), '127.0.0.1');",
            pid = pid_file.to_string_lossy()
        );
        fake_wrapper(tmp.path(), "headed", &body);
        let registry = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        let manager = WrapperManager::new(registry);
        manager.spawn("w").await.expect("the fake wrapper spawns");
        let detached: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Precondition: it really is outside the wrapper's process group.
        let group = |pid: u32| unsafe { libc::getpgid(pid as i32) };
        let wrapper_pid = manager.status("w").await.pid.unwrap();
        assert_ne!(group(detached), group(wrapper_pid));

        manager.stop("w").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(detached) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !alive(detached),
            "detached grandchild {detached} survived stop"
        );
    }

    /// Stop SIGTERMs the group first and waits, so a wrapper's own shutdown
    /// handler (Playwright closing its browser) gets to run.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_lets_the_wrapper_shut_down_gracefully() {
        if !node_available() {
            eprintln!("skipped: node is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("graceful");
        let body = format!(
            "process.on('SIGTERM', () => {{ require('fs').writeFileSync({m:?}, 'closed'); process.exit(0); }});\n\
             require('http').createServer((q, r) => r.end('ok')).listen(Number(process.env.WRAPPER_PORT), '127.0.0.1');",
            m = marker.to_string_lossy()
        );
        fake_wrapper(tmp.path(), "headless", &body);
        let registry = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        let manager = WrapperManager::new(registry);
        manager.spawn("w").await.expect("the fake wrapper spawns");
        manager.stop("w").await.unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "closed");
    }

    #[tokio::test]
    async fn the_drain_survives_bad_utf8_and_cuts_long_lines() {
        let tail = OutputTail::default();
        let mut data: Vec<u8> = b"[wrapper] event=a code=first\n".to_vec();
        data.extend_from_slice(&[0xff, 0xfe, b'b', b'a', b'd', b'\n']);
        data.extend(std::iter::repeat_n(b'A', 100_000));
        data.extend_from_slice(b"\n[wrapper] event=a code=after_long\n[wrapper] event=a code=last");
        tail.drain("w", std::io::Cursor::new(data));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !tail.render().contains("code=last") && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let out = tail.render();
        assert!(out.contains("code=first"), "{out}");
        assert!(
            out.contains("omitted(len="),
            "the bad-UTF-8 line is kept as evidence: {out}"
        );
        assert!(!out.contains("bad"), "{out}");
        assert!(out.contains("[line cut]"), "{out}");
        assert!(out.contains("code=after_long"), "it kept reading: {out}");
        assert!(out.contains("code=last"), "{out}");
        assert!(out.len() <= OUTPUT_ERROR_MAX + OUTPUT_TAIL_LINES * 4);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_descendant_whose_start_time_matches_is_killed() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        Descendant {
            pid,
            start_time: proc_start_time(pid),
        }
        .kill_if_same();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    /// `terminate_runtime` on a record whose leader is ALREADY REAPED must not
    /// signal its old process group: by then that pgid can name an unrelated
    /// group. Here the group still holds a live member (the stand-in for a
    /// reused group); the old code's group kill would take it, the disarm
    /// leaves it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_reaped_leaders_group_is_not_signalled() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("member.pid");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "sleep 30 & echo $! > {}; exit 0",
            pid_file.to_string_lossy()
        ));
        ChildTreeGuard::arm_tokio(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = ChildTreeGuard::attach_armed_tokio(&child);
        child.wait().await.unwrap(); // the leader exits and is REAPED
        assert!(child.id().is_none());
        let member: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(member), "precondition: the group still has a member");

        let now = Instant::now();
        let runtime = WrapperRuntime {
            id: "w".to_string(),
            port: 1,
            pid: None,
            child: Some(child),
            tree: Some(tree),
            output: OutputTail::default(),
            started_at: now,
            last_dispatch: now,
            consecutive_failures: 0,
            state: WrapperState::Running,
        };
        terminate_runtime("w", runtime).await;
        let survived = alive(member);
        // SAFETY: clean up the test's own sleep.
        unsafe { libc::kill(member as i32, libc::SIGKILL) };
        assert!(
            survived,
            "the stale group was signalled after its leader was reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stat_fields_are_read_after_the_last_paren() {
        // comm "a) b (c" holds spaces and parens; ppid is 42, starttime 777.
        let stat = "123 (a) b (c) S 42 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 777 0 0";
        assert_eq!(parse_stat(stat), Some((42, 777)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_descendant_is_killed_only_while_its_start_time_matches() {
        let me = std::process::id() as i32;
        let now = proc_start_time(me).expect("own start time readable");
        // A snapshot whose start time does not match is never signalled (this
        // test process would die if it were).
        Descendant {
            pid: me,
            start_time: Some(now + 1),
        }
        .kill_if_same();
        Descendant {
            pid: me,
            start_time: None,
        }
        .kill_if_same();
        assert_eq!(proc_start_time(me), Some(now));
    }

    #[test]
    fn only_the_allowlist_survives_wrapper_output() {
        // Node's util.inspect of real Set-Cookie headers: the array spans
        // lines, and the values on the continuation lines are short.
        let inspect = [
            "  'set-cookie': [",
            "    'HSID=AbC1; Path=/; Domain=.google.com',",
            "    'SSID=xY2; Path=/; Secure'",
            "  ],",
        ];
        // An escaped JSON body, a Map arrow with an object value, a Bearer.
        // The JWT-shaped value is assembled from pieces so the source holds no
        // secret-shaped literal for a scanner to flag.
        let bearer_line = format!(
            "sending Authorization: Bearer {}.{}.{}",
            concat!("eyJhbGci", "OiJIUzI1NiJ9"),
            concat!("eyJzdWIi", "OiIxIn0"),
            "sig"
        );
        let others = [
            r#"{\"headers\":{\"cookie\":\"SID=s3cr3t\"}}"#,
            "Map(1) { 'authorization' => { value: 'Basic dXNlcjpwYXNz' } }",
            "    value: 'g.a000s3cr3t'",
            bearer_line.as_str(),
            "went to https://accounts.example/signin?continue=SECRET now",
        ];
        for line in inspect.iter().chain(others.iter()) {
            let out = classify_line(line);
            assert!(out.starts_with("omitted(len="), "{line} -> {out}");
            for secret in [
                "HSID",
                "SSID",
                "set-cookie",
                "SID=",
                "s3cr3t",
                "dXNlcjpwYXNz",
                "eyJ",
                "SECRET",
                "authorization",
            ] {
                assert!(!out.contains(secret), "{line} -> {out}");
            }
        }
        // The diagnostics a human needs survive, extracted.
        let out = classify_line(
            "page.goto: net::ERR_CONNECTION_TIMED_OUT at https://gemini.google.com/app?x=1",
        );
        assert!(out.starts_with("net::ERR_CONNECTION_TIMED_OUT ("), "{out}");
        assert!(!out.contains("https"), "{out}");
        let out = classify_line(
            "Error [ERR_PACKAGE_PATH_NOT_EXPORTED]: Package subpath './x' is not defined",
        );
        assert!(out.contains("ERR_PACKAGE_PATH_NOT_EXPORTED"), "{out}");
        assert!(out.contains("Error"), "{out}");
        assert!(!out.contains("subpath"), "{out}");
        let out = classify_line("browserType.launch: Executable doesn't exist at /home/u/.cache/ms-playwright/chromium_headless_shell-1169/chrome");
        assert!(out.starts_with("playwright:executable_missing"), "{out}");
        assert!(!out.contains("/home"), "{out}");
        assert!(classify_line("node exited with code 3").starts_with("exit_status=3"));
        // The structured diagnostic is kept verbatim; one carrying free text,
        // a URL or a quoted value is not.
        assert_eq!(
            classify_line(
                "[wrapper] event=page_load_failed code=net_error detail=net::ERR_NAME_NOT_RESOLVED"
            ),
            "[wrapper] event=page_load_failed code=net_error detail=net::ERR_NAME_NOT_RESOLVED"
        );
        for bad in [
            "[wrapper] event=ready code=ok detail=hello world",
            "[wrapper] event=x code=y detail=https://a.example/?q=1",
            "[wrapper] event=x code=y detail='SID=abc'",
            "[wrapper] event=x code=y extra=1",
        ] {
            assert!(classify_line(bad).starts_with("omitted(len="), "{bad}");
        }
    }

    /// A wrapper that exits at once (a missing peer, no display) fails the
    /// spawn quickly, and the error carries the wrapper's own last words —
    /// with any URL query cut — instead of a bare readiness timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dying_wrapper_fails_fast_with_its_output() {
        if !node_available() {
            eprintln!("skipped: node is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        fake_wrapper(
            tmp.path(),
            "headed",
            "console.error('[wrapper] event=spawn_failed code=missing_peer'); console.error('see https://docs.example/x?token=SECRET'); process.exit(1);",
        );
        let registry = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        let manager = WrapperManager::new(registry);

        let started = Instant::now();
        let err = manager
            .spawn("w")
            .await
            .expect_err("a dying wrapper must not spawn");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited {:?}",
            started.elapsed()
        );
        assert!(err.contains("exited first (exit status 1)"), "{err}");
        assert!(
            err.contains("[wrapper] event=spawn_failed code=missing_peer"),
            "{err}"
        );
        assert!(err.contains("omitted(len="), "{err}");
        assert!(
            !err.contains("SECRET") && !err.contains("docs.example"),
            "{err}"
        );
    }

    #[test]
    fn browser_transports_are_spawnable_and_the_others_are_not() {
        for kind in ["api", "headless", "headed"] {
            assert!(transport_is_spawnable(kind), "{kind} must spawn");
        }
        for kind in ["injected", "live", "", "API", "headed "] {
            assert!(!transport_is_spawnable(kind), "{kind:?} must be refused");
        }
    }

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
                tree: None,
                output: OutputTail::default(),
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
