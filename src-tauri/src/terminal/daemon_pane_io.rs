//! `DaemonPaneIo` — a [`PaneIo`] whose PTY is owned by an out-of-process
//! holder (`qontinui-pty-holder`), so the pane's child can outlive this runner.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2. D2: the holder is the THIRD `PaneIo` impl, not a new subsystem —
//! the reader thread, grid, scrollback ring, emission gate and waiter thread
//! above it are the code a `LocalPty` drives. D13: one holder per pane. Shipped
//! behind `terminal.pty_holder` (default OFF, `settings::TerminalSettings`);
//! [`pane_backend_for`] is the one place that switch is read.
//!
//! # What is reused, and what is new
//!
//! - **Reused wholesale (D2):** the offset-anchored output channel, its
//!   gap-honest splice and in-band loss marker, and the exit slot `wait` parks
//!   on — [`super::pane_output::PaneOutput`], the machinery `RemotePaneIo`
//!   was built on, extracted so both panes hold one. Every output frame is
//!   spliced at the absolute offset the holder stamped on it, so a gap (the
//!   holder's ring rolled past bytes before this connection read them) is
//!   written into the pane as a marker, never silently.
//! - **A new per-connection sink** ([`HolderFrameSink`] / [`HolderConnSink`])
//!   in place of `RemoteFrameSink`, which is `try_send` onto a PROCESS-WIDE
//!   singleton (`OnceLock<RemoteAttachClient>`) drained by the relay's pump,
//!   and which carries JSON with base64 input. The trait shape is kept — the
//!   pane talks only to a sink, a test hands it a recorder — but the client is
//!   this pane's OWN attached holder connection, and input travels as raw
//!   bytes (plan Phase 1: raw bytes on hot paths).
//! - **The local correlation key** is the pane id (the terminal id): it keys
//!   the holder's lock/endpoint/spec files, the [`PaneOutput`] log
//!   attribution, and every log line here — the positions `RemotePaneIo` keys
//!   by `grant_jti`.
//!
//! # Exit-code mapping (`wait`)
//!
//! | what happened | `wait` |
//! |---|---|
//! | `exit {code: Some(c)}` | `Ok(c)` — the real code |
//! | `exit {code: None, signal: Some(_)}` | `Ok(1)` — `LocalPty`'s convention for any non-success, which is what a signal death is there |
//! | `exit {code: None, signal: None}` | `Err(..)` — UNKNOWN; the session records `None`, never a number |
//! | the holder connection ends with no `exit` (holder SIGKILLed / crashed) | `Err(..)` naming the holder pid, plus an in-band notice in the tab — never `DETACH_EXIT_CODE`, never a fabricated child exit |
//! | local detach (`release` without `kill`) | `Ok(DETACH_EXIT_CODE)` — the child is still running, as for `RemotePaneIo` |
//! | `kill` sent, then `release` before the holder reported the exit | `Err(..)` — killed, exit unobserved: UNKNOWN |
//!
//! Holder death is detected by the attached socket reaching EOF, which the
//! kernel produces the moment the holder's descriptors close — so `wait`
//! settles promptly on a SIGKILL without polling.
//!
//! # Close semantics
//!
//! `kill` sends `kill`: closing a tab still ends its process tree, exactly as
//! for `LocalPty` (the holder kills its own child and reports the exit).
//! `release` WITHOUT a prior `kill` sends `detach` — the child keeps running in
//! its holder and a later [`DaemonPaneIo::attach_existing`] can reattach.
//! Runner SHUTDOWN still routes through `TerminalManager::close_all` →
//! `kill`, so in Phase 2 a holder pane does not yet outlive a GRACEFUL runner
//! exit; it outlives a crash or SIGKILL of the runner. Routing shutdown to a
//! detach needs the reattach-on-boot sweep (plan Phase 3) — until then a
//! detached-at-shutdown holder would be an orphan nobody adopts.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{
    connect, probe, ConnectError, Event, Probe, ShutdownHandle, StreamReader, StreamWriter,
};
use qontinui_pty_holder::protocol::{ExitReply, Reply, Request};
use qontinui_runner_lib::pty_holder::spawn::{
    pane_dir_in, resolve_holder_exe, spawn_pane_holder, PaneId, Unprotected,
};
use tracing::{debug, info, warn};

use super::pane_io::{CredentialScrub, PaneIo, ScrubbedCommand};
use super::pane_output::PaneOutput;
use super::remote_pane_io::{DetachOutcome, DETACH_EXIT_CODE};
use crate::settings::TerminalSettings;

/// Bound on connecting to a freshly spawned (or existing) holder and reading
/// its `attached` reply.
pub const ATTACH_DEADLINE: Duration = Duration::from_secs(10);

/// Bound on one control write (`resize`, `pause`, `resume`) by the pane's
/// control thread. The holder's dispatch never blocks on the PTY (its writer
/// thread does), so this long means a wedged holder or a connection jammed
/// behind input.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(10);

/// Bound on one input write. Input waits as long as the CHILD is slow to read
/// (backpressure, as for a local PTY), but not forever: past this the
/// connection is ended and the pump reattaches, so the pane never wedges.
pub const INPUT_DEADLINE: Duration = Duration::from_secs(60);

/// How long `release` tries the `detach` frame before ending the connection
/// instead (which the holder treats the same way).
pub const DETACH_FRAME_BUDGET: Duration = Duration::from_millis(500);

/// How often the control thread re-checks for a settled pane, and its backoff
/// after a control write failed.
const CONTROL_POLL: Duration = Duration::from_millis(250);

/// How long the pump keeps trying to reach a holder that still holds its
/// lock after the connection ended, before it terminates it. Long, because
/// ending a live session is the destructive answer: a loaded box has answered
/// a `/health` probe in 10 s, and a holder busy for a minute is not a dead
/// one. Short enough that a wedged holder is not an invisible orphan forever.
pub const RECONNECT_WINDOW: Duration = Duration::from_secs(120);

/// Bound on one liveness probe while reconnecting.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Pause between reconnect attempts.
const RECONNECT_BACKOFF: Duration = Duration::from_millis(250);

/// Output chunks (each at most the holder's 64 KiB frame) queued between the
/// pump and the session's reader thread. Bounded, so an output flood the
/// reader cannot keep up with backs up into the HOLDER's ring — which reports
/// what it can no longer hold as `output_lost` — instead of into this
/// process's memory.
pub const OUTPUT_QUEUE_CHUNKS: usize = 256;

/// Where the first attach to a freshly spawned holder starts: the stream's
/// first byte. The holder's default for a fresh consumer (`None`) is the ring's
/// last 64 KiB, right for a late viewer and wrong here — a child that writes
/// more than that between its spawn and this attach (the scope route can take
/// tens of seconds to report ready) would lose its first output with no marker.
pub const FRESH_SPAWN_FROM_OFFSET: Option<u64> = Some(0);

/// Largest input chunk per data frame — far under the frame cap, small enough
/// that one paste never holds the writer lock for long.
pub const MAX_INPUT_CHUNK: usize = 64 * 1024;

/// Longest `release` waits for the exit that a preceding `kill` asked for
/// before it detaches and settles the exit as unknown: the holder's worst case
/// is `SIGHUP` → [`KILL_GRACE`](qontinui_pty_holder::pty::KILL_GRACE) →
/// `SIGKILL`, then up to [`EXIT_DRAIN_GRACE`](qontinui_pty_holder::pty::EXIT_DRAIN_GRACE)
/// for the output to drain before the `exit` frame, plus delivery margin.
pub const KILL_SETTLE_MAX: Duration = Duration::from_secs(
    qontinui_pty_holder::pty::KILL_GRACE.as_secs()
        + qontinui_pty_holder::pty::EXIT_DRAIN_GRACE.as_secs()
        + 2,
);

/// `wait`'s answer for a signal death: `LocalPty`'s "non-success is 1".
pub const SIGNAL_EXIT_CODE: i32 = 1;

/// Which byte source a new LOCAL pane gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneBackend {
    /// In-process `portable_pty` ([`super::pane_io::LocalPty`]).
    LocalPty,
    /// An out-of-process `qontinui-pty-holder` ([`DaemonPaneIo`]).
    Holder,
}

/// The `terminal.pty_holder` switch, read at the one place a local pane's
/// backend is chosen (`TerminalSession::spawn`). OFF → `LocalPty`.
pub fn pane_backend_for(settings: &TerminalSettings) -> PaneBackend {
    if settings.pty_holder {
        PaneBackend::Holder
    } else {
        PaneBackend::LocalPty
    }
}

/// The holder executable, resolved ONCE per process (beside the runner's own
/// exe) and reused for every spawn — `current_exe()` at spawn time breaks
/// after an in-place rebuild (Phase 0 hand-off). A failed resolution is cached
/// too: a holder missing at the first spawn is missing for this process.
pub fn holder_exe() -> Result<&'static Path, String> {
    static EXE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match EXE.get_or_init(resolve_holder_exe) {
        Ok(p) => Ok(p.as_path()),
        Err(e) => Err(e.clone()),
    }
}

/// This runner instance's pane directory:
/// `instance::scope_path(<runner dir>)/pty-panes` (plan D13 — namespaced per
/// instance, resolved by the runner and passed to the holder whole).
pub fn runner_pane_dir() -> Result<PathBuf, String> {
    let runner_dir = qontinui_runner_lib::ambient::runner_dir()
        .ok_or_else(|| "no runner data dir (no home directory?)".to_string())?;
    Ok(pane_dir_in(&crate::instance::scope_path(&runner_dir)))
}

/// In-band notice for a pane that asked for a holder and got an in-process
/// PTY instead.
pub fn fallback_notice(reason: &str) -> Vec<u8> {
    format!(
        "\x1b[1;33m[qontinui] terminal.pty_holder is on, but this pane's PTY holder could \
         not be started ({reason}) — it runs inside the runner and will NOT survive a \
         runner exit\x1b[0m\r\n"
    )
    .into_bytes()
}

/// Build a holder pane for terminal `terminal_id`, or — when the holder cannot
/// be started — an in-process `LocalPty` that SAYS so in-band.
///
/// **Fallback policy: fall back, visibly.** With `terminal.pty_holder` on, a
/// holder that cannot start (binary missing from this build, pane dir
/// unusable, spawn or attach refused) could either fail the tab or give the
/// operator today's in-process terminal. Capability ranks first: refusing the
/// tab removes a working terminal to protect a survival property the operator
/// can be told they do not have, and every backend spawn surface (agent
/// sessions, gate continuations) would turn a holder fault into a session that
/// never starts. So the pane falls back to `LocalPty` — never silently: a
/// `warn!` names the reason, and the reason is the first thing the pane itself
/// prints. Only the fallback's own `openpty`/spawn failure fails the tab, with
/// both reasons.
pub fn spawn_holder_pane_or_fallback(
    terminal_id: &str,
    cmd: ScrubbedCommand,
    cols: u16,
    rows: u16,
) -> Result<Arc<dyn PaneIo>, String> {
    off_the_async_workers(|| {
        spawn_holder_pane_or_fallback_in(
            holder_exe(),
            runner_pane_dir(),
            terminal_id,
            cmd,
            cols,
            rows,
        )
    })
}

/// Run `f` — which can block for the holder's ready line, its attach and a
/// fallback spawn — without parking a tokio worker. Most spawn doors already
/// call in from a blocking thread (`terminal_create` uses
/// `spawn_blocking_tracked`); several HTTP/relay doors reach
/// `TerminalManager::create` straight from an async task, and
/// `block_in_place` hands that worker's queue to another thread for the
/// duration. Off a runtime (or on a current-thread one, where
/// `block_in_place` would panic) it runs inline.
fn off_the_async_workers<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// [`spawn_holder_pane_or_fallback`] with the holder executable and pane
/// directory supplied — the seam a test uses so it never resolves the
/// operator's real `~/.qontinui/runner`.
fn spawn_holder_pane_or_fallback_in(
    exe: Result<&Path, String>,
    pane_dir: Result<PathBuf, String>,
    terminal_id: &str,
    cmd: ScrubbedCommand,
    cols: u16,
    rows: u16,
) -> Result<Arc<dyn PaneIo>, String> {
    let attempt = exe.and_then(|exe| {
        let pane_dir = pane_dir?;
        let pane_id = PaneId::new(terminal_id).map_err(|e| e.to_string())?;
        DaemonPaneIo::spawn(exe, &pane_dir, &pane_id, &cmd, cols, rows)
    });
    let reason = match attempt {
        Ok(pane) => return Ok(Arc::new(pane)),
        Err(e) => e,
    };
    warn!(
        terminal_id,
        reason = %reason,
        "pty holder: terminal.pty_holder is on but the holder could not be started — \
         FALLING BACK to an in-process PTY; this pane will not survive a runner exit"
    );
    let local = super::pane_io::LocalPty::open(terminal_id, cols, rows)
        .and_then(|opened| opened.spawn(cmd))
        .map_err(|e| {
            format!("PTY holder unavailable ({reason}); in-process fallback failed: {e}")
        })?;
    Ok(Arc::new(WithNotice {
        notice: fallback_notice(&reason),
        inner: local,
    }))
}

/// A [`PaneIo`] that prints `notice` before the inner pane's first byte and
/// otherwise IS the inner pane.
pub struct WithNotice<P> {
    notice: Vec<u8>,
    inner: P,
}

impl<P: PaneIo> PaneIo for WithNotice<P> {
    fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
        let inner = self.inner.reader()?;
        Ok(Box::new(
            std::io::Cursor::new(self.notice.clone()).chain(inner),
        ))
    }
    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        self.inner.writer()
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.inner.resize(cols, rows)
    }
    fn wait(&self) -> Result<i32, String> {
        self.inner.wait()
    }
    fn kill(&self, budget: Duration) -> Result<(), String> {
        self.inner.kill(budget)
    }
    fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.inner.set_paused(paused)
    }
    fn pid(&self) -> Option<u32> {
        self.inner.pid()
    }
    fn job_enroll_pid(&self) -> Option<u32> {
        self.inner.job_enroll_pid()
    }
    fn unwatched_pauses_source(&self) -> bool {
        self.inner.unwatched_pauses_source()
    }
    fn credential_scrub(&self) -> CredentialScrub {
        self.inner.credential_scrub()
    }
    fn release(&self, budget: Duration) -> Result<(), String> {
        self.inner.release(budget)
    }
}

/// In-band loss notice, the holder's wording of `remote_pane_io`'s marker.
pub fn holder_lost_output_marker(lost_bytes: u64) -> Vec<u8> {
    format!(
        "\r\n\x1b[1;33m[qontinui] {lost_bytes} bytes of output were lost here — the PTY \
         holder's ring rolled past them before this tab read them\x1b[0m\r\n"
    )
    .into_bytes()
}

/// In-band notice for a holder that went away without reporting its child's
/// exit (SIGKILL, crash). Names the holder: the tab must say WHAT died.
pub fn holder_gone_notice(holder_pid: u32, child_pid: u32) -> Vec<u8> {
    format!(
        "\r\n\x1b[1;31m[qontinui] the PTY holder for this pane (pid {holder_pid}) is gone — \
         its connection closed without reporting how pane process {child_pid} exited, so \
         the exit code is unknown\x1b[0m\r\n"
    )
    .into_bytes()
}

/// In-band notice for a holder that held its lock but stopped answering for
/// the whole reconnect window and was terminated.
pub fn holder_terminated_notice(holder_pid: u32, how: &str) -> Vec<u8> {
    format!(
        "\r\n\x1b[1;31m[qontinui] the PTY holder for this pane (pid {holder_pid}) stopped \
         answering, and this tab gave up on it — terminating it: {how}. The pane has ended \
         here and its exit code is unknown\x1b[0m\r\n"
    )
    .into_bytes()
}

/// In-band notice for a pane whose holder will NOT survive the runner's
/// systemd unit stopping (`Unprotected::Cgroup`).
pub fn unprotected_notice(why: &Unprotected) -> Vec<u8> {
    format!(
        "\x1b[1;33m[qontinui] this pane's PTY holder shares the runner's systemd unit \
         ({why}) — it will NOT survive that unit stopping or restarting\x1b[0m\r\n"
    )
    .into_bytes()
}

/// A control request to a pane's holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderControl {
    Resize { cols: u16, rows: u16 },
    Pause,
    Resume,
    Kill,
    Detach,
}

/// Where a holder pane's outbound traffic goes: its holder in production, a
/// recorder in a test. The per-pane replacement for `RemoteFrameSink` (see the
/// module docs).
pub trait HolderLink: Send + Sync {
    /// Raw input bytes for the child, on the attached connection.
    fn input(&self, bytes: &[u8]) -> Result<(), String>;
    /// One control request on the attached connection (`resize`, `pause`,
    /// `resume`, `detach`), bounded by `budget`. Its `ok` arrives on the
    /// reading half and is not waited for.
    fn control(&self, op: HolderControl, budget: Duration) -> Result<(), String>;
    /// `kill`, on a FRESH connection of its own, bounded by `budget` — so a
    /// kill never queues behind input stuck on the attached connection (a
    /// child that does not read its stdin), and works after that connection
    /// failed. `Ok` means the holder answered `ok {kill}`.
    fn kill(&self, budget: Duration) -> Result<(), String>;
    /// End the attached connection without a frame (it may be wedged
    /// mid-write). The holder keeps the child; the pump decides what follows.
    fn abort_connection(&self);
}

/// The production link: the writing half of THIS pane's current attached
/// connection (replaced on every reattach), plus the address to open fresh
/// connections to. A failed write leaves that connection's stream position
/// unknown, so the first failure ENDS the connection — the pump then finds the
/// holder alive and reattaches at the last offset, or finds it gone.
pub struct HolderConn {
    key: String,
    pane_dir: PathBuf,
    pane_id: PaneId,
    writer: Mutex<Option<StreamWriter>>,
    shutdown: Mutex<Option<ShutdownHandle>>,
}

impl HolderConn {
    fn new(pane_dir: &Path, pane_id: &PaneId) -> Self {
        Self {
            key: pane_id.to_string(),
            pane_dir: pane_dir.to_path_buf(),
            pane_id: pane_id.clone(),
            writer: Mutex::new(None),
            shutdown: Mutex::new(None),
        }
    }

    /// Install a newly attached connection's writing half.
    fn install(&self, writer: StreamWriter) {
        let handle = writer.shutdown_handle().ok();
        *self.shutdown.lock().unwrap_or_else(|p| p.into_inner()) = handle;
        *self.writer.lock().unwrap_or_else(|p| p.into_inner()) = Some(writer);
    }

    fn with_writer(
        &self,
        lock_budget: Duration,
        write_budget: Duration,
        f: impl FnOnce(&mut StreamWriter, Instant) -> Result<(), ConnectError>,
    ) -> Result<(), String> {
        let Some(mut slot) =
            crate::safe_lock::lock_with_deadline(&self.writer, "pty holder writer", lock_budget)
        else {
            return Err(format!(
                "pty holder {}: writer busy for longer than {lock_budget:?}",
                self.key
            ));
        };
        let Some(writer) = slot.as_mut() else {
            return Err(format!(
                "pty holder {}: no live connection (reattaching or ended)",
                self.key
            ));
        };
        match f(writer, Instant::now() + write_budget) {
            Ok(()) => Ok(()),
            Err(e) => {
                *slot = None;
                drop(slot);
                self.abort_connection();
                Err(format!(
                    "pty holder {}: write failed ({e}); connection ended for a reattach",
                    self.key
                ))
            }
        }
    }
}

impl HolderLink for HolderConn {
    fn input(&self, bytes: &[u8]) -> Result<(), String> {
        // Waits as long as the CHILD takes to read (the holder queues input to
        // its own PTY writer thread and pushes back only when that queue is
        // full) — the same contract as a local PTY write — but bounded, and a
        // timeout ends this connection rather than wedging the pane.
        self.with_writer(INPUT_DEADLINE, INPUT_DEADLINE, |w, deadline| {
            for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
                w.input(chunk, deadline)?;
            }
            Ok(())
        })
    }

    fn control(&self, op: HolderControl, budget: Duration) -> Result<(), String> {
        self.with_writer(budget, budget, |w, deadline| match op {
            HolderControl::Resize { cols, rows } => w.resize(cols, rows, deadline),
            HolderControl::Pause => w.pause(deadline),
            HolderControl::Resume => w.resume(deadline),
            HolderControl::Kill => w.kill(deadline),
            HolderControl::Detach => w.detach(deadline),
        })
    }

    fn kill(&self, budget: Duration) -> Result<(), String> {
        let deadline = Instant::now() + budget;
        let mut client = connect(&self.pane_dir, &self.pane_id, deadline)
            .map_err(|e| format!("pty holder {}: kill connection failed: {e}", self.key))?;
        match client.request(&Request::Kill, deadline) {
            Ok(Reply::Ok { .. }) => Ok(()),
            Ok(other) => Err(format!(
                "pty holder {}: kill answered {other:?}",
                self.key
            )),
            Err(e) => Err(format!("pty holder {}: kill failed: {e}", self.key)),
        }
    }

    fn abort_connection(&self) {
        if let Some(h) = self
            .shutdown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            h.shutdown();
        }
    }
}

/// What the per-pane control thread still has to send. Coalesced: only the
/// latest size and the latest pause state matter.
#[derive(Debug, Default)]
struct ControlPending {
    resize: Option<(u16, u16)>,
    paused: Option<bool>,
    closed: bool,
}

/// Fire-and-forget `resize` / `pause` / `resume` (finding: a Tauri command or
/// a reader thread must never block on a holder write). One thread per pane
/// sends the latest wanted state; `resize`/`set_paused` only record it.
struct ControlQueue {
    pending: Mutex<ControlPending>,
    cv: Condvar,
}

impl ControlQueue {
    fn new() -> Self {
        Self {
            pending: Mutex::new(ControlPending::default()),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ControlPending> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.lock().resize = Some((cols, rows));
        self.cv.notify_all();
    }

    fn paused(&self, paused: bool) {
        self.lock().paused = Some(paused);
        self.cv.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    /// The control thread: send whatever is pending, newest wins, until the
    /// queue is closed or the pane has settled.
    fn run(&self, link: &dyn HolderLink, out: &PaneOutput, key: &str) {
        loop {
            let (resize, paused) = {
                let mut p = self.lock();
                loop {
                    if p.closed || out.is_finished() {
                        return;
                    }
                    if p.resize.is_some() || p.paused.is_some() {
                        break;
                    }
                    p = match self.cv.wait_timeout(p, CONTROL_POLL) {
                        Ok((g, _)) => g,
                        Err(e) => e.into_inner().0,
                    };
                }
                (p.resize.take(), p.paused.take())
            };
            let mut failed = false;
            if let Some((cols, rows)) = resize {
                if let Err(e) = link.control(HolderControl::Resize { cols, rows }, WRITE_DEADLINE)
                {
                    debug!(pane = %key, error = %e, "pty holder: resize not delivered; retrying");
                    self.lock().resize.get_or_insert((cols, rows));
                    failed = true;
                }
            }
            if let Some(paused) = paused {
                let op = if paused {
                    HolderControl::Pause
                } else {
                    HolderControl::Resume
                };
                if let Err(e) = link.control(op, WRITE_DEADLINE) {
                    debug!(pane = %key, error = %e, "pty holder: flow not delivered; retrying");
                    self.lock().paused.get_or_insert(paused);
                    failed = true;
                }
            }
            if failed {
                // A newer request (already in the slot) wins over the retry;
                // the pump's reattach re-queues the current state anyway.
                std::thread::sleep(CONTROL_POLL);
            }
        }
    }
}

/// A [`PaneIo`] over one pane's PTY holder. See the module docs.
pub struct DaemonPaneIo {
    pane_dir: PathBuf,
    pane_id: PaneId,
    holder_pid: u32,
    child_pid: u32,
    out: Arc<PaneOutput>,
    link: Arc<dyn HolderLink>,
    control: Arc<ControlQueue>,
    /// The holder answered a `kill`: an exit that is not reported before
    /// `release` is UNKNOWN, not a detach. Set only once the kill landed.
    killed: AtomicBool,
    /// Set BEFORE the detach goes out, so the pump reads the holder closing
    /// the connection as the detach it is, never as a holder death, and never
    /// reattaches.
    detached: Arc<AtomicBool>,
    detach: Arc<Mutex<DetachOutcome>>,
    cols: Arc<AtomicU16>,
    rows: Arc<AtomicU16>,
    /// The pause state last asked for, re-sent on a reattach (a new
    /// connection starts unpaused).
    paused: Arc<AtomicBool>,
    unprotected: Option<Unprotected>,
    /// How long after a `kill` the pane keeps its connection open for the
    /// holder's `exit` report before it detaches and records the exit as
    /// unknown. [`KILL_SETTLE_MAX`] in production; a test shortens it.
    kill_settle: Duration,
}

/// Everything the pump thread needs to reattach.
struct PumpCtx {
    pane_dir: PathBuf,
    pane_id: PaneId,
    key: String,
    holder_pid: u32,
    child_pid: u32,
    out: Arc<PaneOutput>,
    conn: Arc<HolderConn>,
    control: Arc<ControlQueue>,
    detached: Arc<AtomicBool>,
    cols: Arc<AtomicU16>,
    rows: Arc<AtomicU16>,
    paused: Arc<AtomicBool>,
}

/// How the pump's connection ended.
enum Reconnect {
    /// Reattached: keep pumping from this reader.
    Again(StreamReader),
    /// Nothing more to pump (detached, settled).
    Done,
}

impl DaemonPaneIo {
    /// Spawn a holder for `pane_id` running `cmd` and attach to it.
    ///
    /// `cmd` is a [`ScrubbedCommand`], and the holder's spec is built from it
    /// by [`ScrubbedCommand::to_holder_spec`] — the only constructor of a
    /// holder spec from a runner command (plan D6). A failure after the
    /// holder reported ready tears it down (child included); nothing is left
    /// behind. The attach asks for offset 0: a fresh pane's every byte so far
    /// is in the ring, and the default fresh-consumer tail (the last 64 KiB)
    /// would silently drop the start of a chatty child's output.
    pub fn spawn(
        holder_exe: &Path,
        pane_dir: &Path,
        pane_id: &PaneId,
        cmd: &ScrubbedCommand,
        cols: u16,
        rows: u16,
    ) -> Result<Self, String> {
        let spec = cmd.to_holder_spec(cols, rows);
        let spawned = spawn_pane_holder(holder_exe, pane_dir, pane_id, &spec)
            .map_err(|e| format!("PTY holder spawn failed: {e}"))?;
        let unprotected = spawned.unprotected.clone();
        match Self::attach_fresh(pane_dir, pane_id, unprotected) {
            Ok(pane) => {
                info!(
                    pane = %pane_id,
                    holder_pid = pane.holder_pid,
                    child_pid = pane.child_pid,
                    route = spawned.route.as_str(),
                    "pty holder: pane spawned and attached"
                );
                spawned.reap_in_background();
                Ok(pane)
            }
            Err(e) => {
                spawned.teardown();
                Err(e)
            }
        }
    }

    /// The first attach to a holder this runner just spawned: from
    /// [`FRESH_SPAWN_FROM_OFFSET`], so everything the child wrote between its
    /// spawn and this attach is delivered — or, if the ring already rolled
    /// past it, reported as `output_lost` — never silently skipped.
    fn attach_fresh(
        pane_dir: &Path,
        pane_id: &PaneId,
        unprotected: Option<Unprotected>,
    ) -> Result<Self, String> {
        Self::attach(pane_dir, pane_id, FRESH_SPAWN_FROM_OFFSET, unprotected)
    }

    /// Reattach to a holder that is already serving `pane_id` — after a
    /// detach, or (Phase 3+) after a runner restart. `from_offset` is the
    /// first byte the caller has not seen (`None`: the ring's tail).
    ///
    /// This side did not watch that holder's spawn; its child spec was built,
    /// like every holder spec, through [`ScrubbedCommand::to_holder_spec`] by
    /// the runner that spawned it, which is why
    /// [`CredentialScrub::ScrubbedOutOfProcess`] is still the honest answer.
    pub fn attach_existing(
        pane_dir: &Path,
        pane_id: &PaneId,
        from_offset: Option<u64>,
    ) -> Result<Self, String> {
        Self::attach(pane_dir, pane_id, from_offset, None)
    }

    fn attach(
        pane_dir: &Path,
        pane_id: &PaneId,
        from_offset: Option<u64>,
        unprotected: Option<Unprotected>,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + ATTACH_DEADLINE;
        let stream = connect(pane_dir, pane_id, deadline)
            .and_then(|c| c.attach(from_offset, deadline))
            .map_err(|e| format!("PTY holder {pane_id}: attach failed: {e}"))?;
        let info = stream.info;
        if let Some(asked) = from_offset {
            if info.start_offset != asked {
                warn!(
                    pane = %pane_id,
                    asked,
                    start_offset = info.start_offset,
                    ring_start = info.ring_start_offset,
                    "pty holder: attach starts at a different offset than requested"
                );
            }
        }
        let key = pane_id.to_string();
        let out = Arc::new(PaneOutput::new_bounded(
            key.clone(),
            info.start_offset,
            holder_lost_output_marker,
            OUTPUT_QUEUE_CHUNKS,
        ));
        if let Some(why) = &unprotected {
            warn!(
                pane = %pane_id,
                holder_pid = stream.hello_ack.holder_pid,
                "pty holder: {why} — this pane will NOT survive the runner's systemd unit stopping"
            );
            out.push_local(&unprotected_notice(why));
        }
        let holder_pid = stream.hello_ack.holder_pid;
        let child_pid = info.child_pid;
        let conn = Arc::new(HolderConn::new(pane_dir, pane_id));
        conn.install(stream.writer);
        let control = Arc::new(ControlQueue::new());
        let detached = Arc::new(AtomicBool::new(false));
        let cols = Arc::new(AtomicU16::new(info.cols));
        let rows = Arc::new(AtomicU16::new(info.rows));
        let paused = Arc::new(AtomicBool::new(false));
        {
            let (control, link, out, key) =
                (control.clone(), conn.clone(), out.clone(), key.clone());
            std::thread::Builder::new()
                .name(format!("pty-holder-ctl-{key}"))
                .spawn(move || control.run(link.as_ref(), &out, &key))
                .map_err(|e| format!("Failed to spawn pty holder control thread: {e}"))?;
        }
        let ctx = PumpCtx {
            pane_dir: pane_dir.to_path_buf(),
            pane_id: pane_id.clone(),
            key,
            holder_pid,
            child_pid,
            out: out.clone(),
            conn: conn.clone(),
            control: control.clone(),
            detached: detached.clone(),
            cols: cols.clone(),
            rows: rows.clone(),
            paused: paused.clone(),
        };
        if let Err(e) = std::thread::Builder::new()
            .name(format!("pty-holder-pump-{}", ctx.key))
            .spawn(move || ctx.pump(stream.reader))
        {
            control.close();
            return Err(format!("Failed to spawn pty holder pump thread: {e}"));
        }
        Ok(Self {
            pane_dir: pane_dir.to_path_buf(),
            pane_id: pane_id.clone(),
            holder_pid,
            child_pid,
            out,
            link: conn,
            control,
            killed: AtomicBool::new(false),
            detached,
            detach: Arc::new(Mutex::new(DetachOutcome::NotAttempted)),
            cols,
            rows,
            paused,
            unprotected,
            kill_settle: KILL_SETTLE_MAX,
        })
    }

    pub fn pane_id(&self) -> &PaneId {
        &self.pane_id
    }

    pub fn holder_pid(&self) -> u32 {
        self.holder_pid
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// Absolute holder-stream offset of the next byte this pane expects — what
    /// a reattach passes as `from_offset`.
    pub fn stream_offset(&self) -> u64 {
        self.out.offset()
    }

    /// `Some` when this pane will NOT survive something a caller might assume
    /// it survives (the holder shares the runner's systemd unit cgroup).
    pub fn unprotected(&self) -> Option<&Unprotected> {
        self.unprotected.as_ref()
    }

    pub fn dims(&self) -> (u16, u16) {
        (
            self.cols.load(Ordering::Relaxed),
            self.rows.load(Ordering::Relaxed),
        )
    }

    pub fn detach_outcome(&self) -> DetachOutcome {
        match self.detach.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// The parts of this pane a deferred post-kill settle needs (see
    /// [`PaneIo::release`] below), detached from `&self`.
    fn detacher(&self) -> Detacher {
        Detacher {
            key: self.pane_id.to_string(),
            slot: self.detach.clone(),
            detached: self.detached.clone(),
            control: self.control.clone(),
            link: self.link.clone(),
        }
    }
}

/// Everything a detach touches, cloneable onto another thread.
struct Detacher {
    key: String,
    slot: Arc<Mutex<DetachOutcome>>,
    detached: Arc<AtomicBool>,
    control: Arc<ControlQueue>,
    link: Arc<dyn HolderLink>,
}

impl Detacher {
    /// Detach unless that already happened: the `detach` frame when the
    /// connection takes it within `budget`, else END the connection — which
    /// is the same thing to the holder (it keeps the child). `detached` is
    /// raised first so the pump neither reads the close as a holder death nor
    /// reattaches.
    fn detach_once(&self, budget: Duration) {
        let mut slot = match self.slot.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *slot == DetachOutcome::Queued {
            return;
        }
        self.detached.store(true, Ordering::Release);
        self.control.close();
        let sent = self
            .link
            .control(HolderControl::Detach, budget.min(DETACH_FRAME_BUDGET));
        if let Err(e) = &sent {
            debug!(pane = %self.key, error = %e, "pty holder: detach frame not sent; ending the connection instead");
        }
        // Either way the connection ends here: the frame makes the holder
        // close it, and a wedged writer is closed under it.
        self.link.abort_connection();
        *slot = DetachOutcome::Queued;
    }
}

/// After a `kill` whose exit was not reported within `release`'s budget: keep
/// the connection open until the holder reports the exit or `left` runs out —
/// so the REAL exit code reaches `wait` and the holder, its `exit` delivered,
/// leaves at once instead of lingering — then detach and record the exit as
/// unknown. Runs on the caller's thread; `release` puts it on its own.
fn settle_after_kill(
    out: &PaneOutput,
    detacher: &Detacher,
    left: Duration,
    holder_pid: u32,
    child_pid: u32,
) {
    if out.wait_for(left).is_some() {
        detacher.control.close();
        return;
    }
    detacher.detach_once(DETACH_FRAME_BUDGET);
    out.settle(Err(format!(
        "kill sent to PTY holder pid {holder_pid} but the exit of pane process {child_pid} \
         was not reported within {left:?} of the pane's release — exit code unknown"
    )));
}

impl PumpCtx {
    /// The reading half: holder events → [`PaneOutput`], across reattaches.
    /// Ends at the child's `exit`, at our own detach, or when the holder is
    /// verifiably gone. A connection that ends while the holder is still
    /// alive is NEVER read as the holder's death (finding: a rejected frame
    /// closes the connection, not the pane): the pump reattaches at the last
    /// offset it delivered.
    fn pump(self, mut reader: StreamReader) {
        loop {
            let ended = loop {
                match reader.next_event(None) {
                    Ok(Some(Event::Output { offset, bytes })) => {
                        self.out.splice(offset, &bytes);
                    }
                    Ok(Some(Event::Lost {
                        from_offset,
                        to_offset,
                    })) => self.out.note_lost(from_offset, to_offset),
                    Ok(Some(Event::Exit(exit))) => {
                        info!(pane = %self.key, holder_pid = self.holder_pid, child_pid = self.child_pid, ?exit, "pty holder: pane process exited");
                        self.out.settle(exit_result(exit));
                        self.control.close();
                        return;
                    }
                    Ok(Some(Event::Reply(Reply::Rejected { reason, detail }))) => {
                        warn!(pane = %self.key, ?reason, %detail, "pty holder: request rejected; the holder closes this connection");
                    }
                    Ok(Some(Event::Reply(_))) => {}
                    Ok(None) => break "end of stream".to_string(),
                    Err(e) => break e.to_string(),
                }
            };
            drop(reader);
            match self.reconnect(&ended) {
                Reconnect::Again(r) => reader = r,
                Reconnect::Done => {
                    self.control.close();
                    return;
                }
            }
        }
    }

    /// The connection ended with no `exit`. Decide from the holder's own
    /// liveness — the answered handshake and the lock (`client::probe`) —
    /// never from the closed socket alone.
    fn reconnect(&self, ended: &str) -> Reconnect {
        let give_up_at = Instant::now() + RECONNECT_WINDOW;
        let mut last = String::new();
        loop {
            if self.detached.load(Ordering::Acquire) || self.out.is_finished() {
                return Reconnect::Done;
            }
            match probe(&self.pane_dir, &self.pane_id, Instant::now() + PROBE_DEADLINE) {
                Probe::Healthy { .. } => {
                    let deadline = Instant::now() + ATTACH_DEADLINE;
                    let from = self.out.offset();
                    match connect(&self.pane_dir, &self.pane_id, deadline)
                        .and_then(|c| c.attach(Some(from), deadline))
                    {
                        Ok(stream) => {
                            if self.detached.load(Ordering::Acquire) {
                                return Reconnect::Done;
                            }
                            info!(pane = %self.key, holder_pid = self.holder_pid, from, reason = %ended, "pty holder: connection ended with the holder alive — reattached");
                            self.conn.install(stream.writer);
                            // A new connection starts unpaused at the holder's
                            // size: re-send what this pane last asked for.
                            self.control.resize(
                                self.cols.load(Ordering::Relaxed),
                                self.rows.load(Ordering::Relaxed),
                            );
                            if self.paused.load(Ordering::Acquire) {
                                self.control.paused(true);
                            }
                            return Reconnect::Again(stream.reader);
                        }
                        Err(e) => last = format!("reattach failed: {e}"),
                    }
                }
                Probe::Dead { .. } | Probe::Absent => {
                    self.settle_gone(ended, "its lock is released — the holder is dead");
                    return Reconnect::Done;
                }
                Probe::Unknown { reason, .. } => last = reason,
                Probe::Incompatible { holder_versions } => {
                    last = format!("answers but speaks {holder_versions:?}")
                }
            }
            if Instant::now() >= give_up_at {
                // Alive (it holds its lock) but unreachable for the whole
                // window. Not "gone": end it, verifiably, then say so.
                let how = terminate_holder(&self.pane_dir, &self.pane_id, self.holder_pid);
                let how_text = how.describe();
                warn!(pane = %self.key, holder_pid = self.holder_pid, last = %last, how = %how_text, "pty holder: unreachable for the reconnect window — terminating it");
                self.out
                    .push_local(&holder_terminated_notice(self.holder_pid, how_text));
                self.out.settle(Err(format!(
                    "PTY holder pid {} for pane {} stopped answering ({last}) for {:?}; \
                     terminating it: {how_text} — exit code of pane process {} unknown",
                    self.holder_pid, self.key, RECONNECT_WINDOW, self.child_pid
                )));
                return Reconnect::Done;
            }
            std::thread::sleep(RECONNECT_BACKOFF);
        }
    }

    fn settle_gone(&self, ended: &str, why: &str) {
        warn!(
            pane = %self.key,
            holder_pid = self.holder_pid,
            child_pid = self.child_pid,
            reason = %ended,
            "pty holder: connection ended without an exit report and {why}; exit code unknown"
        );
        self.out
            .push_local(&holder_gone_notice(self.holder_pid, self.child_pid));
        self.out.settle(Err(format!(
            "PTY holder pid {} for pane {} ended ({ended}; {why}) without reporting how \
             pane process {} exited — exit code unknown",
            self.holder_pid, self.key, self.child_pid
        )));
    }
}

/// What [`terminate_holder`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HolderTermination {
    /// A kill was actually delivered to the verified holder.
    signalled: bool,
    detail: String,
}

impl HolderTermination {
    fn not_signalled(detail: impl Into<String>) -> Self {
        Self {
            signalled: false,
            detail: format!("not signalled: {}", detail.into()),
        }
    }

    fn signalled(&self) -> bool {
        self.signalled
    }

    fn describe(&self) -> &str {
        &self.detail
    }
}

/// End a holder that holds its lock but answers nothing. Signals ONLY a pid
/// that is verified to still be this pane's holder: the pane's lock must be
/// HELD right now (an acquirable lock is a dead holder, whose pid may already
/// be recycled), its record must name the SAME holder pid this pane attached
/// to, and the pid must be in the signalable range (Phase 0 hand-off:
/// "teardown signals only verified pids"). The holder's death ends its child:
/// the PTY hangs up (Unix), the holder's job closes (Windows).
fn terminate_holder(pane_dir: &Path, pane_id: &PaneId, holder_pid: u32) -> HolderTermination {
    let lock_file = qontinui_pty_holder::pane::lock_path(pane_dir, pane_id);
    match qontinui_pty_holder::lock::PaneLock::try_acquire_existing(&lock_file) {
        Ok(qontinui_pty_holder::lock::TryLock::Held) => {}
        Ok(qontinui_pty_holder::lock::TryLock::Acquired(lock)) => {
            drop(lock);
            return HolderTermination::not_signalled("the pane's lock is free — the holder is already dead");
        }
        Err(e) => return HolderTermination::not_signalled(format!("lock unreadable: {e}")),
    }
    match qontinui_pty_holder::lock::read_record(&lock_file) {
        Some(r) if r.holder_pid == holder_pid => {}
        Some(r) => {
            return HolderTermination::not_signalled(format!(
                "the lock names holder pid {}, not {holder_pid}",
                r.holder_pid
            ))
        }
        None => return HolderTermination::not_signalled("no lock record to verify the pid against"),
    }
    let Some(pid) = qontinui_pty_holder::spawn::signalable_pid(holder_pid) else {
        return HolderTermination::not_signalled(format!("pid {holder_pid} out of range"));
    };
    #[cfg(unix)]
    {
        // SAFETY: a plain signal to the pid this pane's HELD lock names.
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            HolderTermination {
                signalled: true,
                detail: "SIGKILL to the holder".into(),
            }
        } else {
            HolderTermination::not_signalled(format!(
                "SIGKILL failed: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
    #[cfg(windows)]
    {
        // `/T`: the holder's child tree goes with it (its job would end it
        // anyway once the holder's last handle closes).
        let mut cmd = crate::process_helpers::no_window("taskkill");
        cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
        match crate::drain::output_with_timeout(cmd, Duration::from_secs(5)) {
            Ok(Some(o)) if o.status.success() => HolderTermination {
                signalled: true,
                detail: "taskkill /F /T of the holder".into(),
            },
            Ok(Some(o)) => HolderTermination::not_signalled(format!(
                "taskkill exited {:?}",
                o.status.code()
            )),
            Ok(None) => HolderTermination::not_signalled("taskkill exceeded 5s"),
            Err(e) => HolderTermination::not_signalled(format!("taskkill could not be spawned: {e}")),
        }
    }
}

/// `wait`'s answer for an `exit` frame. See the module docs' table.
pub fn exit_result(exit: ExitReply) -> Result<i32, String> {
    match (exit.code, exit.signal) {
        (Some(code), _) => Ok(code),
        (None, Some(_)) => Ok(SIGNAL_EXIT_CODE),
        (None, None) => Err(
            "the pane process exited and its holder could not observe the \
             exit code — exit code unknown"
                .to_string(),
        ),
    }
}

/// `Write` that ships each write as raw input through the link.
struct HolderInputWriter {
    link: Arc<dyn HolderLink>,
}

impl Write for HolderInputWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.link.input(buf).map_err(std::io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PaneIo for DaemonPaneIo {
    fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
        self.out.take_reader()
    }

    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        Ok(Box::new(HolderInputWriter {
            link: self.link.clone(),
        }))
    }

    /// Fire-and-forget: recorded and sent by the pane's control thread, so a
    /// synchronous `terminal_resize` never waits on the holder. Clamped to
    /// 1×1 — a PTY has no zero dimension, and the holder answers a zero with
    /// `rejected {malformed}` and closes the connection.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        if !self.out.is_finished() {
            self.control.resize(cols, rows);
        }
        Ok(())
    }

    fn wait(&self) -> Result<i32, String> {
        self.out.wait()
    }

    /// `kill` on a fresh connection, bounded by `budget`. `killed` is set only
    /// once the holder answered it — a kill that never landed must not turn a
    /// later release into "killed, exit unknown".
    fn kill(&self, budget: Duration) -> Result<(), String> {
        if self.out.is_finished() {
            return Ok(());
        }
        // The holder kills its own child's tree and reports the real exit,
        // which settles `wait`; nothing is fabricated here.
        if let Err(asked) = self.link.kill(budget) {
            // The holder did not answer a kill on a fresh connection of its
            // own. Leaving it would orphan the child behind a closed tab, so
            // end the holder itself — only once it is verified to be the
            // holder this pane attached to — which hangs up the child's PTY
            // (Unix) or closes the job holding its tree (Windows).
            let how = terminate_holder(&self.pane_dir, &self.pane_id, self.holder_pid);
            warn!(
                pane = %self.pane_id,
                holder_pid = self.holder_pid,
                error = %asked,
                how = %how.describe(),
                "pty holder: kill not answered — terminating the holder out of band"
            );
            if !how.signalled() {
                return Err(format!("{asked}; holder not terminated: {}", how.describe()));
            }
        }
        self.killed.store(true, Ordering::Release);
        Ok(())
    }

    /// Projects the consumer's single hysteresis machine (`WireFlow` over the
    /// `EmissionGate`) onto the holder's per-connection `pause`/`resume`,
    /// fire-and-forget through the control thread. The holder never pauses its
    /// PTY read; it withholds SENDING and resumes from this connection's own
    /// offset, so a resume needs no resync.
    fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.paused.store(paused, Ordering::Release);
        if !self.out.is_finished() {
            self.control.paused(paused);
        }
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child_pid)
    }

    /// Never enrolled in the runner's crash-safety Job Object: that job is
    /// `KILL_ON_JOB_CLOSE`, so enrolling would end the child exactly when the
    /// runner exits — the event the holder exists to survive. Reaping is the
    /// holder's (plan D8).
    fn job_enroll_pid(&self) -> Option<u32> {
        None
    }

    /// The runner is where this pane's state tracking happens (grid,
    /// auto-response, needs-input), so an UNWATCHED tier must keep the bytes
    /// flowing — unlike a remote pane, whose state is tracked on its target.
    fn unwatched_pauses_source(&self) -> bool {
        false
    }

    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::ScrubbedOutOfProcess
    }

    fn release(&self, budget: Duration) -> Result<(), String> {
        if self.out.is_finished() {
            self.control.close();
            self.out.close_output();
            return Ok(());
        }
        if self.killed.load(Ordering::Acquire) {
            let waited = budget.min(self.kill_settle);
            if self.out.wait_for(waited).is_some() {
                self.control.close();
                return Ok(());
            }
            // `budget` is the caller's LOCK budget (2 s interactive, less at
            // shutdown) and is shorter than the holder's worst case for a
            // kill (`KILL_GRACE` + `EXIT_DRAIN_GRACE`). Do not let it decide
            // the exit code: end the reader now (release's contract) and
            // settle the exit in the background — the real code if the
            // holder reports it within `kill_settle`, else unknown.
            self.out.close_output();
            let (out, detacher) = (self.out.clone(), self.detacher());
            let left = self.kill_settle.saturating_sub(waited);
            let (holder_pid, child_pid) = (self.holder_pid, self.child_pid);
            let spawned = std::thread::Builder::new()
                .name(format!("pty-holder-settle-{}", self.pane_id))
                .spawn(move || settle_after_kill(&out, &detacher, left, holder_pid, child_pid));
            if spawned.is_err() {
                settle_after_kill(
                    &self.out,
                    &self.detacher(),
                    Duration::ZERO,
                    self.holder_pid,
                    self.child_pid,
                );
            }
            return Ok(());
        }
        self.detacher().detach_once(budget);
        self.out.settle(Ok(DETACH_EXIT_CODE));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records everything a pane sends.
    #[derive(Default)]
    struct RecordingLink {
        input: Mutex<Vec<u8>>,
        controls: Mutex<Vec<HolderControl>>,
        aborts: std::sync::atomic::AtomicUsize,
        fail_kill: AtomicBool,
    }

    impl HolderLink for RecordingLink {
        fn input(&self, bytes: &[u8]) -> Result<(), String> {
            self.input.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn control(&self, op: HolderControl, _budget: Duration) -> Result<(), String> {
            self.controls.lock().unwrap().push(op);
            Ok(())
        }
        fn kill(&self, _budget: Duration) -> Result<(), String> {
            if self.fail_kill.load(Ordering::SeqCst) {
                return Err("holder unreachable".into());
            }
            self.controls.lock().unwrap().push(HolderControl::Kill);
            Ok(())
        }
        fn abort_connection(&self) {
            self.aborts.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl RecordingLink {
        fn controls(&self) -> Vec<HolderControl> {
            self.controls.lock().unwrap().clone()
        }
        /// Wait (bounded) until the control thread has sent `op`.
        fn saw(&self, op: HolderControl) {
            let end = Instant::now() + Duration::from_secs(5);
            while !self.controls().contains(&op) {
                assert!(Instant::now() < end, "{op:?} never sent: {:?}", self.controls());
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn recorded_pane() -> (Arc<RecordingLink>, DaemonPaneIo) {
        let link = Arc::new(RecordingLink::default());
        let pane = pane_with_link(link.clone());
        (link, pane)
    }

    /// A pane over `link`, with its control thread running and no holder.
    fn pane_with_link(link: Arc<dyn HolderLink>) -> DaemonPaneIo {
        let out = Arc::new(PaneOutput::new(
            "t-1",
            Vec::new(),
            0,
            holder_lost_output_marker,
        ));
        let control = Arc::new(ControlQueue::new());
        {
            let (control, link, out) = (control.clone(), link.clone(), out.clone());
            std::thread::spawn(move || control.run(link.as_ref(), &out, "t-1"));
        }
        DaemonPaneIo {
            // Never created: a kill fallback finds no lock here and signals
            // nothing.
            pane_dir: std::env::temp_dir().join("qontinui-daemon-pane-io-test-no-such-dir"),
            pane_id: PaneId::new("t-1").unwrap(),
            holder_pid: 100,
            child_pid: 101,
            out,
            link,
            control,
            killed: AtomicBool::new(false),
            detached: Arc::new(AtomicBool::new(false)),
            detach: Arc::new(Mutex::new(DetachOutcome::NotAttempted)),
            cols: Arc::new(AtomicU16::new(80)),
            rows: Arc::new(AtomicU16::new(24)),
            paused: Arc::new(AtomicBool::new(false)),
            unprotected: None,
            kill_settle: KILL_SETTLE_MAX,
        }
    }

    /// The setting OFF (the default) yields `LocalPty`; ON yields the holder.
    #[test]
    fn pty_holder_backend_off_by_default_yields_local_pty() {
        assert_eq!(
            pane_backend_for(&TerminalSettings::default()),
            PaneBackend::LocalPty
        );
        assert_eq!(
            pane_backend_for(&TerminalSettings { pty_holder: true }),
            PaneBackend::Holder
        );
    }

    /// Exit frames map to the real code; a signal to `LocalPty`'s 1; an
    /// unknown code to `Err` — never 0.
    #[test]
    fn pty_holder_exit_frame_mapping() {
        assert_eq!(
            exit_result(ExitReply {
                code: Some(3),
                signal: None
            }),
            Ok(3)
        );
        assert_eq!(
            exit_result(ExitReply {
                code: Some(0),
                signal: None
            }),
            Ok(0)
        );
        assert_eq!(
            exit_result(ExitReply {
                code: None,
                signal: Some(9)
            }),
            Ok(SIGNAL_EXIT_CODE)
        );
        assert!(exit_result(ExitReply {
            code: None,
            signal: None
        })
        .is_err());
    }

    /// Every control call maps to its frame, sent by the control thread
    /// (fire-and-forget: the call itself returns at once); `release` without
    /// `kill` is a detach that also ends the connection and settles `wait`
    /// with the detach code.
    #[test]
    fn pty_holder_control_calls_map_to_holder_frames() {
        let (link, pane) = recorded_pane();
        let mut w = pane.writer().unwrap();
        w.write_all(b"ls\r").unwrap();
        pane.resize(132, 50).unwrap();
        link.saw(HolderControl::Resize {
            cols: 132,
            rows: 50,
        });
        pane.set_paused(true).unwrap();
        link.saw(HolderControl::Pause);
        pane.set_paused(false).unwrap();
        link.saw(HolderControl::Resume);
        pane.release(Duration::from_millis(10)).unwrap();
        pane.release(Duration::from_millis(10)).unwrap();
        assert_eq!(link.input.lock().unwrap().as_slice(), b"ls\r");
        assert_eq!(
            link.controls()
                .iter()
                .filter(|c| **c == HolderControl::Detach)
                .count(),
            1,
            "one detach"
        );
        assert_eq!(link.aborts.load(Ordering::SeqCst), 1, "the detach ends the connection");
        assert_eq!(pane.dims(), (132, 50));
        assert_eq!(pane.wait(), Ok(DETACH_EXIT_CODE));
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
        assert_eq!(
            pane.credential_scrub(),
            CredentialScrub::ScrubbedOutOfProcess
        );
        assert_eq!(pane.job_enroll_pid(), None);
        assert_eq!(pane.pid(), Some(101));
        assert!(!pane.unwatched_pauses_source());
    }

    /// Review finding 1: a zero dimension (the webview can report one) is
    /// clamped to 1, never sent — the holder rejects a zero and closes.
    #[test]
    fn pty_holder_zero_resize_is_clamped_to_one() {
        let (link, pane) = recorded_pane();
        pane.resize(0, 0).unwrap();
        link.saw(HolderControl::Resize { cols: 1, rows: 1 });
        assert_eq!(pane.dims(), (1, 1));
        pane.release(Duration::from_millis(10)).unwrap();
    }

    /// `kill` asks the holder and fabricates nothing; a `release` before the
    /// exit is reported returns at once, and once the settle window passes
    /// with no exit it detaches and settles the exit as UNKNOWN, not 0.
    #[test]
    fn pty_holder_kill_then_release_without_exit_is_unknown_not_detach() {
        let (link, mut pane) = recorded_pane();
        pane.kill_settle = Duration::from_millis(200);
        pane.kill(Duration::from_millis(10)).unwrap();
        assert!(
            !pane.out.is_finished(),
            "kill must not settle the exit itself"
        );
        let started = Instant::now();
        pane.release(Duration::from_millis(20)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(150), "release does not wait out the settle window");
        let exit = pane.wait();
        assert!(exit.is_err(), "{exit:?}");
        assert_eq!(
            link.controls(),
            vec![HolderControl::Kill, HolderControl::Detach]
        );
    }

    /// Review finding 7: the exit a holder reports AFTER `release`'s (short,
    /// lock-sized) budget but within the settle window still reaches `wait`
    /// as the real code — the pane neither detaches early nor records an
    /// unknown exit — and the reader is ended at release, not later.
    #[test]
    fn pty_holder_exit_reported_after_release_budget_is_the_real_code() {
        let (link, pane) = recorded_pane();
        let pane = Arc::new(pane);
        let mut reader = pane.reader().unwrap();
        pane.kill(Duration::from_millis(10)).unwrap();
        let reporter = {
            let pane = pane.clone();
            std::thread::spawn(move || {
                // Later than release's budget, as a holder's SIGHUP grace is.
                std::thread::sleep(Duration::from_millis(300));
                pane.out.settle(exit_result(ExitReply {
                    code: None,
                    signal: Some(9),
                }));
            })
        };
        pane.release(Duration::from_millis(20)).unwrap();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(pane.wait(), Ok(SIGNAL_EXIT_CODE), "the real exit, not unknown");
        reporter.join().unwrap();
        assert_eq!(
            link.controls(),
            vec![HolderControl::Kill],
            "no detach: the connection stays up for the exit report"
        );
    }

    /// Review finding 5: `resize` and `set_paused` never wait on the holder —
    /// a link whose writes hang (a wedged holder) does not hold the caller
    /// (`terminal_resize` runs on the Tauri main thread).
    #[test]
    fn pty_holder_resize_and_pause_do_not_block_on_a_wedged_link() {
        struct WedgedLink {
            gate: Mutex<()>,
            entered: AtomicBool,
        }
        impl HolderLink for WedgedLink {
            fn input(&self, _bytes: &[u8]) -> Result<(), String> {
                Ok(())
            }
            fn control(&self, _op: HolderControl, _budget: Duration) -> Result<(), String> {
                self.entered.store(true, Ordering::SeqCst);
                drop(self.gate.lock().unwrap());
                Ok(())
            }
            fn kill(&self, _budget: Duration) -> Result<(), String> {
                Ok(())
            }
            fn abort_connection(&self) {}
        }
        let link = Arc::new(WedgedLink {
            gate: Mutex::new(()),
            entered: AtomicBool::new(false),
        });
        let held = link.gate.lock().unwrap();
        let pane = pane_with_link(link.clone());
        let started = Instant::now();
        pane.resize(100, 40).unwrap();
        // Wait until the control thread is inside the wedged write.
        let end = Instant::now() + Duration::from_secs(5);
        while !link.entered.load(Ordering::SeqCst) {
            assert!(Instant::now() < end, "control thread never sent");
            std::thread::sleep(Duration::from_millis(5));
        }
        let during = Instant::now();
        pane.resize(120, 50).unwrap();
        pane.set_paused(true).unwrap();
        pane.set_paused(false).unwrap();
        assert!(
            during.elapsed() < Duration::from_millis(100),
            "resize/pause waited on a wedged holder: {:?}",
            during.elapsed()
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(pane.dims(), (120, 50));
        drop(held);
        pane.release(Duration::from_millis(10)).unwrap();
    }

    /// Review finding 5: a holder spawn reached from an async task does not
    /// park the tokio worker — another task on a ONE-worker runtime still
    /// runs while the blocking spawn is in progress. Off a runtime, and on a
    /// current-thread one (where `block_in_place` would panic), it runs inline.
    #[test]
    fn pty_holder_spawn_does_not_park_a_tokio_worker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let ran_meanwhile = rt.block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let started = Arc::new(AtomicBool::new(false));
            let (seen, began) = (flag.clone(), started.clone());
            let blocking = tokio::spawn(async move {
                off_the_async_workers(|| {
                    began.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(400));
                    seen.load(Ordering::SeqCst)
                })
            });
            // `block_on` runs this future on the test thread, not on the one
            // worker, so this wait cannot itself hold the worker.
            while !started.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let other = tokio::spawn(async move {
                flag.store(true, Ordering::SeqCst);
            });
            other.await.unwrap();
            blocking.await.unwrap()
        });
        assert!(ran_meanwhile, "the other task waited for the blocking spawn");
        assert_eq!(off_the_async_workers(|| 7), 7);
        let current = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(current.block_on(async { off_the_async_workers(|| 8) }), 8);
    }

    /// Review finding 2: the out-of-band kill signals only a VERIFIED holder:
    /// a free lock (dead holder, pid possibly recycled) and a lock naming a
    /// different pid are refused; a held lock naming the pid is killed.
    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_terminate_signals_only_a_verified_holder() {
        use qontinui_pty_holder::lock::{LockRecord, PaneLock, TryLock};
        let dir = std::env::temp_dir().join(format!(
            "qontinui-term-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        qontinui_pty_holder::pane::prepare_private_dir(&dir).unwrap();
        let id = PaneId::new("t").unwrap();
        let lock_file = qontinui_pty_holder::pane::lock_path(&dir, &id);
        // A stand-in "holder" this test owns.
        let mut victim = std::process::Command::new("sleep") // console-ok: a Linux-only test's own child process
            .arg("30")
            .spawn()
            .unwrap();
        let record = |pid: u32| LockRecord {
            holder_pid: pid,
            child_pid: None,
            versions: vec![2],
            started_at_unix_ms: 0,
            holder_build: "test".into(),
        };

        // Lock free: the holder is dead — never signal.
        {
            let TryLock::Acquired(mut lock) = PaneLock::try_acquire(&lock_file).unwrap() else {
                panic!("fresh lock");
            };
            lock.write_record(&record(victim.id())).unwrap();
        }
        let t = terminate_holder(&dir, &id, victim.id());
        assert!(!t.signalled(), "{t:?}");
        assert!(victim.try_wait().unwrap().is_none(), "a free lock's pid was signalled");

        let TryLock::Acquired(mut lock) = PaneLock::try_acquire(&lock_file).unwrap() else {
            panic!("lock");
        };
        // Held, but naming another pid.
        lock.write_record(&record(victim.id() + 1)).unwrap();
        let t = terminate_holder(&dir, &id, victim.id());
        assert!(!t.signalled(), "{t:?}");
        assert!(victim.try_wait().unwrap().is_none());

        // Held and naming it: killed.
        lock.write_record(&record(victim.id())).unwrap();
        let t = terminate_holder(&dir, &id, victim.id());
        assert!(t.signalled(), "{t:?}");
        let end = Instant::now() + Duration::from_secs(5);
        while victim.try_wait().unwrap().is_none() {
            assert!(Instant::now() < end, "the verified holder was not killed");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review finding 2: a kill that never landed does not mark the pane
    /// killed — the release that follows is an honest detach.
    #[test]
    fn pty_holder_failed_kill_is_not_recorded_as_sent() {
        let (link, pane) = recorded_pane();
        link.fail_kill.store(true, Ordering::SeqCst);
        assert!(pane.kill(Duration::from_millis(10)).is_err());
        pane.release(Duration::from_millis(10)).unwrap();
        assert_eq!(pane.wait(), Ok(DETACH_EXIT_CODE));
    }

    /// The exit the holder reports after a kill wins over the release.
    #[test]
    fn pty_holder_kill_then_reported_exit_settles_with_it() {
        let (link, pane) = recorded_pane();
        pane.kill(Duration::from_millis(10)).unwrap();
        pane.out.settle(exit_result(ExitReply {
            code: None,
            signal: Some(15),
        }));
        pane.release(Duration::from_millis(10)).unwrap();
        assert_eq!(link.controls(), vec![HolderControl::Kill]);
        assert_eq!(pane.wait(), Ok(SIGNAL_EXIT_CODE));
    }

    /// Review finding 7: the settle window covers the holder's worst case.
    #[test]
    fn pty_holder_kill_settle_window_covers_the_holders_worst_case() {
        assert!(
            KILL_SETTLE_MAX
                > qontinui_pty_holder::pty::KILL_GRACE + qontinui_pty_holder::pty::EXIT_DRAIN_GRACE
        );
    }

    /// With no holder available, the switch-on path falls back to an
    /// in-process PTY and the pane SAYS so before its first byte.
    #[test]
    fn pty_holder_unavailable_falls_back_to_local_pty_with_a_notice() {
        let mut cmd = portable_pty::CommandBuilder::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            cmd.args(["/C", "echo FALLBACK_MARK"]);
        } else {
            cmd.args(["-c", "echo FALLBACK_MARK"]);
        }
        let pane = spawn_holder_pane_or_fallback_in(
            Err("no holder in this test".into()),
            Err("unused".into()),
            "fallback-term",
            ScrubbedCommand::seal(cmd),
            80,
            24,
        )
        .expect("fallback spawns");
        assert_eq!(pane.credential_scrub(), CredentialScrub::InProcessEnv);
        let mut reader = pane.reader().unwrap();
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink = collected.clone();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        assert_eq!(pane.wait(), Ok(0));
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end
            && !String::from_utf8_lossy(&collected.lock().unwrap()).contains("FALLBACK_MARK")
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        pane.release(Duration::from_secs(2)).unwrap();
        let _ = t.join();
        let got = collected.lock().unwrap().clone();
        let notice = fallback_notice("no holder in this test");
        assert!(
            got.starts_with(&notice),
            "notice first: {:?}",
            String::from_utf8_lossy(&got)
        );
        assert!(String::from_utf8_lossy(&got).contains("FALLBACK_MARK"));
    }

    /// End to end against the real `qontinui-pty-holder`. Linux only: the
    /// children are POSIX shell scripts driving `stty`, and holder death is a
    /// SIGKILL. The Windows (named pipe / ConPTY) and macOS arms are UNRUN
    /// here, not passed.
    #[cfg(target_os = "linux")]
    mod holder_e2e {
        use super::*;

        /// The holder binary cargo built into this target dir (the test exe
        /// is `<target>/<profile>/deps/…`). Missing is a FAILURE that says how
        /// to build it, never a skip: a skipped end-to-end test is a vacuous
        /// pass.
        fn holder() -> PathBuf {
            let exe = std::env::current_exe().unwrap();
            let profile = exe.parent().and_then(Path::parent).unwrap();
            let p = profile.join("qontinui-pty-holder");
            assert!(
                p.is_file(),
                "{} is missing — build it first: `cargo build -p qontinui-pty-holder` \
                 (CI's Linux test job does this before `cargo test`)",
                p.display()
            );
            p
        }

        /// A short pane dir under /tmp (a Unix socket path is capped at ~108 bytes).
        fn pane_dir(tag: &str) -> PathBuf {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            PathBuf::from("/tmp").join(format!("ptyd-{tag}-{}-{nanos:x}", std::process::id()))
        }

        fn sh(script: &str) -> ScrubbedCommand {
            let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
            cmd.args(["-c", script]);
            cmd.cwd("/tmp");
            ScrubbedCommand::seal(cmd)
        }

        fn alive(pid: u32) -> bool {
            // SAFETY: signal 0 only checks for existence.
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }

        /// Drain a pane's reader on a thread into a shared buffer.
        fn drain(pane: &DaemonPaneIo) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
            let mut reader = pane.reader().expect("reader");
            let collected = Arc::new(Mutex::new(Vec::new()));
            let sink = collected.clone();
            let t = std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
            (collected, t)
        }

        fn until(collected: &Mutex<Vec<u8>>, bound: Duration, done: impl Fn(&[u8]) -> bool) {
            let end = Instant::now() + bound;
            loop {
                if done(&collected.lock().unwrap()) {
                    return;
                }
                assert!(
                    Instant::now() < end,
                    "condition not met within {bound:?}; collected {:?}",
                    String::from_utf8_lossy(&collected.lock().unwrap())
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn contains(hay: &[u8], needle: &[u8]) -> bool {
            hay.windows(needle.len()).any(|w| w == needle)
        }

        /// `wait` on a thread, bounded.
        fn wait_within(pane: &Arc<DaemonPaneIo>, bound: Duration) -> Result<i32, String> {
            let (tx, rx) = std::sync::mpsc::channel();
            let p = pane.clone();
            std::thread::spawn(move || {
                let _ = tx.send(p.wait());
            });
            rx.recv_timeout(bound)
                .unwrap_or_else(|_| panic!("wait did not settle within {bound:?}"))
        }

        /// Input typed into a `DaemonPaneIo` reaches the holder's child, the
        /// child's output reaches `DaemonPaneIo::reader`, and the child's real
        /// exit code reaches `wait`.
        #[test]
        fn pty_holder_daemon_pane_input_reaches_child_and_output_reaches_reader() {
            let dir = pane_dir("io");
            let id = PaneId::new("io-pane").unwrap();
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &id,
                    &sh("stty -echo; printf READY; read line; echo \"got:$line\"; exit 7"),
                    100,
                    30,
                )
                .expect("spawn + attach"),
            );
            assert_eq!(
                pane.credential_scrub(),
                CredentialScrub::ScrubbedOutOfProcess
            );
            assert!(alive(pane.child_pid()));
            assert_eq!(pane.job_enroll_pid(), None);
            let (collected, t) = drain(&pane);
            until(&collected, Duration::from_secs(10), |c| {
                contains(c, b"READY")
            });
            pane.writer().unwrap().write_all(b"hello-holder\r").unwrap();
            until(&collected, Duration::from_secs(10), |c| {
                contains(c, b"got:hello-holder")
            });
            assert_eq!(
                wait_within(&pane, Duration::from_secs(10)),
                Ok(7),
                "the real exit code"
            );
            t.join().unwrap();
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Byte fidelity end to end: holder PTY → output frame →
        /// `PaneOutput` → `DaemonPaneIo::reader`, for all 256 byte values and
        /// invalid UTF-8, in raw mode (the line discipline would otherwise
        /// translate — see the holder crate's own fidelity test). The input
        /// direction travels the same way and is checked through a file.
        #[test]
        fn pty_holder_daemon_pane_byte_fidelity_all_256_and_invalid_utf8() {
            let files = pane_dir("fidf");
            std::fs::create_dir_all(&files).unwrap();
            let mut payload: Vec<u8> = (0u8..=255).collect();
            payload.extend_from_slice(&[
                0x80, 0xBF, 0xC0, 0xAF, 0xF0, 0x9F, 0x98, 0xED, 0xA0, 0x80, 0xFF, 0xFE,
            ]);
            payload.extend((0u8..=255).rev());
            let src = files.join("src.bin");
            let got_in = files.join("in.bin");
            std::fs::write(&src, &payload).unwrap();
            let script = format!(
                "stty raw -echo; printf R; head -c {} > '{}'; cat '{}'",
                payload.len(),
                got_in.display(),
                src.display()
            );
            let dir = pane_dir("fid");
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &PaneId::new("fid").unwrap(),
                    &sh(&script),
                    80,
                    24,
                )
                .expect("spawn + attach"),
            );
            let (collected, t) = drain(&pane);
            until(&collected, Duration::from_secs(10), |c| c.ends_with(b"R"));
            let mut w = pane.writer().unwrap();
            for chunk in payload.chunks(37) {
                w.write_all(chunk).unwrap();
            }
            assert_eq!(wait_within(&pane, Duration::from_secs(15)), Ok(0));
            t.join().unwrap();
            // A holder that could not escape the runner's cgroup prints its
            // in-band warning first; everything after it is the child's.
            let mut expected = pane
                .unprotected()
                .map(unprotected_notice)
                .unwrap_or_default();
            expected.extend_from_slice(b"R");
            expected.extend_from_slice(&payload);
            assert_eq!(
                *collected.lock().unwrap(),
                expected,
                "the child's PTY output reached DaemonPaneIo::reader byte-identical"
            );
            assert_eq!(
                std::fs::read(&got_in).unwrap(),
                payload,
                "input reached the child byte-identical"
            );
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&files);
        }

        /// SIGKILL the holder under a live pane: `wait` settles within a bound,
        /// with neither `DETACH_EXIT_CODE` nor a fabricated child code (it is
        /// UNKNOWN — `Err`), the tab gets a notice naming the holder, and the
        /// reader reaches EOF — nothing hangs.
        #[test]
        fn pty_holder_sigkill_of_holder_settles_wait_promptly_and_honestly() {
            let dir = pane_dir("kill");
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &PaneId::new("kill").unwrap(),
                    &sh("printf UP; exec sleep 3600"),
                    80,
                    24,
                )
                .expect("spawn + attach"),
            );
            let (collected, t) = drain(&pane);
            until(&collected, Duration::from_secs(10), |c| contains(c, b"UP"));
            let holder_pid = pane.holder_pid();
            let child_pid = pane.child_pid();
            // SAFETY: the holder pid came from this pane's own handshake.
            assert_eq!(unsafe { libc::kill(holder_pid as i32, libc::SIGKILL) }, 0);
            let started = Instant::now();
            let exit = wait_within(&pane, Duration::from_secs(5));
            assert!(started.elapsed() < Duration::from_secs(5));
            match &exit {
                Err(e) => assert!(e.contains(&holder_pid.to_string()), "{e}"),
                Ok(code) => panic!("holder death fabricated exit code {code}"),
            }
            t.join().expect("the reader reached EOF");
            let notice = holder_gone_notice(holder_pid, child_pid);
            assert!(
                contains(&collected.lock().unwrap(), &notice),
                "the tab names the holder: {:?}",
                String::from_utf8_lossy(&collected.lock().unwrap())
            );
            pane.release(Duration::from_millis(100)).unwrap();
            // The holder's death hung up the child's PTY; make sure either way.
            if alive(child_pid) {
                // SAFETY: the child pid came from this pane's own attach reply.
                unsafe {
                    libc::kill(child_pid as i32, libc::SIGKILL);
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// `release` without `kill` detaches: `wait` reports the detach code,
        /// the child keeps running in its holder, and a FRESH `DaemonPaneIo`
        /// reattaches at the offset the first one reached — input still reaches
        /// the same child. A reattach that knows its holder is unprotected says
        /// so in-band. `kill` then ends the child with a real (signal) exit.
        #[test]
        fn pty_holder_detach_leaves_child_alive_and_a_fresh_pane_reattaches() {
            let dir = pane_dir("re");
            let id = PaneId::new("re").unwrap();
            let first = Arc::new(
                DaemonPaneIo::spawn(&holder(), &dir, &id, &sh("stty -echo; exec cat"), 80, 24)
                    .expect("spawn + attach"),
            );
            let child_pid = first.child_pid();
            let (c1, t1) = drain(&first);
            first.writer().unwrap().write_all(b"one\r").unwrap();
            until(&c1, Duration::from_secs(10), |c| contains(c, b"one"));
            first.release(Duration::from_secs(2)).unwrap();
            assert_eq!(first.detach_outcome(), DetachOutcome::Queued);
            assert_eq!(
                wait_within(&first, Duration::from_secs(5)),
                Ok(DETACH_EXIT_CODE)
            );
            t1.join().unwrap();
            let offset = first.stream_offset();
            std::thread::sleep(Duration::from_millis(200));
            assert!(alive(child_pid), "detach must leave the child running");

            let second =
                Arc::new(DaemonPaneIo::attach_existing(&dir, &id, Some(offset)).expect("reattach"));
            assert_eq!(second.child_pid(), child_pid, "the SAME child");
            assert_eq!(
                second.stream_offset(),
                offset,
                "resumes where the first left off"
            );
            let (c2, t2) = drain(&second);
            second.writer().unwrap().write_all(b"two\r").unwrap();
            until(&c2, Duration::from_secs(10), |c| contains(c, b"two"));
            assert!(
                !contains(&c2.lock().unwrap(), b"one"),
                "no duplicate replay"
            );
            second.release(Duration::from_secs(2)).unwrap();
            assert_eq!(
                wait_within(&second, Duration::from_secs(5)),
                Ok(DETACH_EXIT_CODE)
            );
            t2.join().unwrap();

            let why = Unprotected::Cgroup {
                fallback_reason: None,
            };
            let third = Arc::new(
                DaemonPaneIo::attach(&dir, &id, Some(second.stream_offset()), Some(why.clone()))
                    .expect("reattach again"),
            );
            assert_eq!(third.unprotected(), Some(&why));
            let (c3, t3) = drain(&third);
            until(&c3, Duration::from_secs(5), |c| {
                c.starts_with(&unprotected_notice(&why))
            });
            third.kill(Duration::from_secs(2)).unwrap();
            assert_eq!(
                wait_within(&third, Duration::from_secs(10)),
                Ok(SIGNAL_EXIT_CODE),
                "kill ends the child; its signal exit is LocalPty's 1, not the detach code"
            );
            t3.join().unwrap();
            third.release(Duration::from_secs(1)).unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            while alive(child_pid) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!alive(child_pid), "kill ended the child");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Review finding 4: a fresh spawn's first attach starts at the
        /// stream's first byte. The child writes ~200 KB — over three times
        /// the holder's 64 KiB fresh-consumer tail — and only THEN is the pane
        /// attached (through the same `attach_fresh` `spawn` uses): its first
        /// bytes still arrive, with no loss marker. Also pins finding 3: the
        /// pane's output queue is the bounded one.
        #[test]
        fn pty_holder_fresh_spawn_attach_delivers_output_written_before_it() {
            let dir = pane_dir("fresh");
            let files = pane_dir("freshf");
            std::fs::create_dir_all(&files).unwrap();
            let done = files.join("done");
            let id = PaneId::new("fresh").unwrap();
            let script = format!(
                "printf START; head -c 200000 /dev/zero | tr '\\0' x; printf END; : > '{}'; exec sleep 3600",
                done.display()
            );
            let spawned = spawn_pane_holder(&holder(), &dir, &id, &sh(&script).to_holder_spec(80, 24))
                .expect("holder spawn");
            let end = Instant::now() + Duration::from_secs(10);
            while !done.exists() {
                assert!(Instant::now() < end, "the child never finished writing");
                std::thread::sleep(Duration::from_millis(10));
            }
            // Let the holder's reader move the PTY's last bytes into its ring.
            std::thread::sleep(Duration::from_millis(300));
            let pane = Arc::new(
                DaemonPaneIo::attach_fresh(&dir, &id, spawned.unprotected.clone())
                    .expect("attach"),
            );
            spawned.reap_in_background();
            assert_eq!(pane.out.capacity(), Some(OUTPUT_QUEUE_CHUNKS));
            let (collected, t) = drain(&pane);
            until(&collected, Duration::from_secs(10), |c| contains(c, b"END"));
            let got = collected.lock().unwrap().clone();
            assert!(
                contains(&got, b"STARTxxxx"),
                "the child's first output was dropped: {:?}",
                String::from_utf8_lossy(&got[..got.len().min(200)])
            );
            assert!(!contains(&got, b"were lost"), "no loss: the ring held it all");
            pane.kill(Duration::from_secs(5)).unwrap();
            assert_eq!(wait_within(&pane, Duration::from_secs(10)), Ok(SIGNAL_EXIT_CODE));
            t.join().unwrap();
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&files);
        }

        /// Write `bytes`, retrying while the pane is between connections
        /// (a write that lands mid-reattach reports "no live connection").
        fn write_retrying(pane: &DaemonPaneIo, bytes: &[u8]) {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                match pane.writer().unwrap().write_all(bytes) {
                    Ok(()) => return,
                    Err(e) => {
                        assert!(Instant::now() < end, "input never landed: {e}");
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }

        /// Review finding 1: neither a zero-size resize nor a connection that
        /// ends while the holder is alive ends the pane. The resize is
        /// clamped; the dropped connection is reattached at the last offset —
        /// no loss marker, no "holder gone", input still reaches the SAME
        /// child — and only a real kill ends it.
        #[test]
        fn pty_holder_dropped_connection_with_a_live_holder_reattaches() {
            let dir = pane_dir("drop");
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &PaneId::new("drop").unwrap(),
                    &sh("stty -echo; exec cat"),
                    80,
                    24,
                )
                .expect("spawn + attach"),
            );
            let (collected, t) = drain(&pane);
            pane.resize(0, 0).unwrap();
            write_retrying(&pane, b"one\r");
            until(&collected, Duration::from_secs(10), |c| contains(c, b"one"));
            assert!(!pane.out.is_finished(), "a zero resize must not end the pane");

            pane.link.abort_connection();
            write_retrying(&pane, b"two\r");
            until(&collected, Duration::from_secs(15), |c| contains(c, b"two"));
            assert!(!pane.out.is_finished(), "a dropped connection is not a holder death");
            let text = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
            assert!(!text.contains("is gone"), "{text:?}");
            assert!(!text.contains("were lost"), "a reattach at the offset loses nothing: {text:?}");
            assert!(alive(pane.child_pid()));

            pane.kill(Duration::from_secs(5)).unwrap();
            assert_eq!(wait_within(&pane, Duration::from_secs(10)), Ok(SIGNAL_EXIT_CODE));
            t.join().unwrap();
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Review finding 2: a child that never reads its stdin plus a paste
        /// far larger than every buffer between us: `kill` still lands (on a
        /// fresh connection, and the holder's dispatch is not stuck behind a
        /// PTY write), `wait` settles with the real signal exit, and the
        /// blocked paste returns instead of hanging.
        #[test]
        fn pty_holder_kill_lands_while_a_big_paste_is_blocked() {
            let dir = pane_dir("paste");
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &PaneId::new("paste").unwrap(),
                    &sh("printf UP; exec sleep 3600"),
                    80,
                    24,
                )
                .expect("spawn + attach"),
            );
            let (collected, t) = drain(&pane);
            until(&collected, Duration::from_secs(10), |c| contains(c, b"UP"));
            let paster = {
                let pane = pane.clone();
                std::thread::spawn(move || {
                    let paste = vec![b'x'; 16 * 1024 * 1024];
                    pane.writer().unwrap().write_all(&paste)
                })
            };
            std::thread::sleep(Duration::from_millis(1500));
            assert!(!paster.is_finished(), "the paste should be blocked on a child that never reads");

            let started = Instant::now();
            pane.kill(Duration::from_secs(5)).expect("kill lands despite the blocked paste");
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(wait_within(&pane, Duration::from_secs(10)), Ok(SIGNAL_EXIT_CODE));
            t.join().unwrap();

            let end = Instant::now() + Duration::from_secs(20);
            while !paster.is_finished() {
                assert!(Instant::now() < end, "the blocked paste never returned after the kill");
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = paster.join();
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
