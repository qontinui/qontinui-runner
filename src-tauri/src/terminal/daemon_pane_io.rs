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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, ConnectError, Event, StreamReader, StreamWriter};
use qontinui_pty_holder::protocol::{ExitReply, Reply};
use qontinui_runner_lib::pty_holder::spawn::{
    pane_dir_in, resolve_holder_exe, spawn_pane_holder, PaneId, Unprotected,
};
use tracing::{info, warn};

use super::pane_io::{CredentialScrub, PaneIo, ScrubbedCommand};
use super::pane_output::PaneOutput;
use super::remote_pane_io::{DetachOutcome, DETACH_EXIT_CODE};
use crate::settings::TerminalSettings;

/// Bound on connecting to a freshly spawned (or existing) holder and reading
/// its `attached` reply.
pub const ATTACH_DEADLINE: Duration = Duration::from_secs(10);

/// Bound on one write to the holder (input or a control frame) when the
/// caller gave none. The holder drains its socket continuously, so a write
/// that takes this long means a wedged holder.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(10);

/// Largest input chunk per data frame — far under the frame cap, small enough
/// that one paste never holds the writer lock for long.
pub const MAX_INPUT_CHUNK: usize = 64 * 1024;

/// Longest `release` waits for the exit that a preceding `kill` asked for
/// before it detaches and settles the exit as unknown.
pub const KILL_SETTLE_MAX: Duration = Duration::from_secs(3);

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
    spawn_holder_pane_or_fallback_in(
        holder_exe(),
        runner_pane_dir(),
        terminal_id,
        cmd,
        cols,
        rows,
    )
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

/// Where a holder pane's outbound frames go: its own attached connection in
/// production, a recorder in a test. The per-connection replacement for
/// `RemoteFrameSink` (see the module docs).
pub trait HolderFrameSink: Send + Sync {
    /// Raw input bytes for the child.
    fn input(&self, bytes: &[u8]) -> Result<(), String>;
    /// One control request, bounded by `budget`. Its `ok` reply arrives on the
    /// reading half and is not waited for.
    fn control(&self, op: HolderControl, budget: Duration) -> Result<(), String>;
}

/// The production sink: the writing half of THIS pane's attached connection.
/// A failed write leaves the stream position unknown, so the first failure
/// drops the writer and every later call reports why.
pub struct HolderConnSink {
    key: String,
    writer: Mutex<Option<StreamWriter>>,
    dead: Mutex<Option<String>>,
}

impl HolderConnSink {
    pub fn new(key: impl Into<String>, writer: StreamWriter) -> Self {
        Self {
            key: key.into(),
            writer: Mutex::new(Some(writer)),
            dead: Mutex::new(None),
        }
    }

    fn with_writer(
        &self,
        budget: Duration,
        f: impl FnOnce(&mut StreamWriter, Instant) -> Result<(), ConnectError>,
    ) -> Result<(), String> {
        let Some(mut slot) =
            crate::safe_lock::lock_with_deadline(&self.writer, "pty holder writer", budget)
        else {
            return Err(format!(
                "pty holder {}: writer busy for longer than {budget:?}",
                self.key
            ));
        };
        let Some(writer) = slot.as_mut() else {
            let why = self
                .dead
                .lock()
                .ok()
                .and_then(|d| d.clone())
                .unwrap_or_else(|| "closed".into());
            return Err(format!(
                "pty holder {}: connection unusable ({why})",
                self.key
            ));
        };
        match f(writer, Instant::now() + budget) {
            Ok(()) => Ok(()),
            Err(e) => {
                let why = e.to_string();
                *slot = None;
                if let Ok(mut d) = self.dead.lock() {
                    *d = Some(why.clone());
                }
                Err(format!("pty holder {}: write failed: {why}", self.key))
            }
        }
    }
}

impl HolderFrameSink for HolderConnSink {
    fn input(&self, bytes: &[u8]) -> Result<(), String> {
        self.with_writer(WRITE_DEADLINE, |w, deadline| {
            for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
                w.input(chunk, deadline)?;
            }
            Ok(())
        })
    }

    fn control(&self, op: HolderControl, budget: Duration) -> Result<(), String> {
        self.with_writer(budget, |w, deadline| match op {
            HolderControl::Resize { cols, rows } => w.resize(cols, rows, deadline),
            HolderControl::Pause => w.pause(deadline),
            HolderControl::Resume => w.resume(deadline),
            HolderControl::Kill => w.kill(deadline),
            HolderControl::Detach => w.detach(deadline),
        })
    }
}

/// A [`PaneIo`] over one pane's PTY holder. See the module docs.
pub struct DaemonPaneIo {
    pane_id: PaneId,
    holder_pid: u32,
    child_pid: u32,
    out: Arc<PaneOutput>,
    sink: Arc<dyn HolderFrameSink>,
    /// A `kill` has been sent: an exit that is not reported before `release`
    /// is UNKNOWN, not a detach.
    killed: AtomicBool,
    /// Set BEFORE the detach frame goes out, so the pump reads the holder
    /// closing the connection as the detach it is, not as a holder death.
    detached: Arc<AtomicBool>,
    detach: Mutex<DetachOutcome>,
    cols: AtomicU16,
    rows: AtomicU16,
    unprotected: Option<Unprotected>,
}

impl DaemonPaneIo {
    /// Spawn a holder for `pane_id` running `cmd` and attach to it.
    ///
    /// `cmd` is a [`ScrubbedCommand`], and the holder's spec is built from it
    /// by [`ScrubbedCommand::to_holder_spec`] — the only constructor of a
    /// holder spec from a runner command (plan D6). A failure after the
    /// holder reported ready tears it down (child included); nothing is left
    /// behind.
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
        match Self::attach(pane_dir, pane_id, None, unprotected) {
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
                    "pty holder: reattach starts at a different offset than requested"
                );
            }
        }
        let key = pane_id.to_string();
        let out = Arc::new(PaneOutput::new(
            key.clone(),
            Vec::new(),
            info.start_offset,
            holder_lost_output_marker,
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
        let detached = Arc::new(AtomicBool::new(false));
        Self::start_pump(
            stream.reader,
            out.clone(),
            detached.clone(),
            holder_pid,
            child_pid,
            key.clone(),
        )?;
        Ok(Self {
            pane_id: pane_id.clone(),
            holder_pid,
            child_pid,
            out,
            sink: Arc::new(HolderConnSink::new(key, stream.writer)),
            killed: AtomicBool::new(false),
            detached,
            detach: Mutex::new(DetachOutcome::NotAttempted),
            cols: AtomicU16::new(info.cols),
            rows: AtomicU16::new(info.rows),
            unprotected,
        })
    }

    /// The reading half's thread: holder events → [`PaneOutput`]. Ends at the
    /// child's `exit`, at EOF, or on a read error; the last two settle the
    /// exit as UNKNOWN (holder gone) unless this side detached first.
    fn start_pump(
        mut reader: StreamReader,
        out: Arc<PaneOutput>,
        detached: Arc<AtomicBool>,
        holder_pid: u32,
        child_pid: u32,
        key: String,
    ) -> Result<(), String> {
        std::thread::Builder::new()
            .name(format!("pty-holder-pump-{key}"))
            .spawn(move || {
                let ended = loop {
                    match reader.next_event(None) {
                        Ok(Some(Event::Output { offset, bytes })) => {
                            out.splice(offset, &bytes);
                        }
                        Ok(Some(Event::Lost {
                            from_offset,
                            to_offset,
                        })) => out.note_lost(from_offset, to_offset),
                        Ok(Some(Event::Exit(exit))) => {
                            info!(pane = %key, holder_pid, child_pid, ?exit, "pty holder: pane process exited");
                            out.settle(exit_result(exit));
                            return;
                        }
                        Ok(Some(Event::Reply(Reply::Rejected { reason, detail }))) => {
                            warn!(pane = %key, ?reason, %detail, "pty holder: request rejected; the holder closes this connection");
                        }
                        Ok(Some(Event::Reply(_))) => {}
                        Ok(None) => break "end of stream".to_string(),
                        Err(e) => break e.to_string(),
                    }
                };
                if detached.load(Ordering::Acquire) {
                    // Our own detach: `release` settles the exit.
                    return;
                }
                warn!(
                    pane = %key,
                    holder_pid,
                    child_pid,
                    reason = %ended,
                    "pty holder: connection ended without an exit report — holder gone; exit code unknown"
                );
                out.push_local(&holder_gone_notice(holder_pid, child_pid));
                out.settle(Err(format!(
                    "PTY holder pid {holder_pid} for pane {key} ended ({ended}) without reporting \
                     how pane process {child_pid} exited — exit code unknown"
                )));
            })
            .map(|_| ())
            .map_err(|e| format!("Failed to spawn pty holder pump thread: {e}"))
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

    /// Send `detach` unless one already went. Only a successful write
    /// latches; `detached` is raised first so the pump never mistakes the
    /// holder closing the connection for its death.
    fn send_detach_once(&self, budget: Duration) -> Result<(), String> {
        let mut slot = match self.detach.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *slot == DetachOutcome::Queued {
            return Ok(());
        }
        self.detached.store(true, Ordering::Release);
        let sent = self.sink.control(HolderControl::Detach, budget);
        *slot = match &sent {
            Ok(()) => DetachOutcome::Queued,
            Err(e) => DetachOutcome::Failed(e.clone()),
        };
        sent
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

/// `Write` that ships each write as raw input through the sink.
struct HolderInputWriter {
    sink: Arc<dyn HolderFrameSink>,
}

impl Write for HolderInputWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.sink.input(buf).map_err(std::io::Error::other)?;
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
            sink: self.sink.clone(),
        }))
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        if self.out.is_finished() {
            // Like a released local master: nothing left to size.
            return Ok(());
        }
        self.sink
            .control(HolderControl::Resize { cols, rows }, WRITE_DEADLINE)
    }

    fn wait(&self) -> Result<i32, String> {
        self.out.wait()
    }

    fn kill(&self, budget: Duration) -> Result<(), String> {
        if self.out.is_finished() {
            return Ok(());
        }
        self.killed.store(true, Ordering::Release);
        // The holder kills its own child and reports the real exit, which
        // settles `wait`; nothing is fabricated here.
        self.sink.control(HolderControl::Kill, budget)
    }

    /// Projects the consumer's single hysteresis machine (`WireFlow` over the
    /// `EmissionGate`) onto the holder's per-connection `pause`/`resume`. The
    /// holder never pauses its PTY read; it withholds SENDING and resumes from
    /// this connection's own offset, so a resume needs no resync.
    fn set_paused(&self, paused: bool) -> Result<(), String> {
        if self.out.is_finished() {
            return Ok(());
        }
        let op = if paused {
            HolderControl::Pause
        } else {
            HolderControl::Resume
        };
        self.sink.control(op, WRITE_DEADLINE)
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
            self.out.close_output();
            return Ok(());
        }
        if self.killed.load(Ordering::Acquire)
            && self.out.wait_for(budget.min(KILL_SETTLE_MAX)).is_some()
        {
            return Ok(());
        }
        let sent = self.send_detach_once(budget);
        if self.killed.load(Ordering::Acquire) {
            self.out.settle(Err(format!(
                "kill sent to PTY holder pid {} but the exit of pane process {} was not \
                 reported before the pane was released — exit code unknown",
                self.holder_pid, self.child_pid
            )));
        } else {
            self.out.settle(Ok(DETACH_EXIT_CODE));
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every frame a pane sends.
    #[derive(Default)]
    struct RecordingSink {
        input: Mutex<Vec<u8>>,
        controls: Mutex<Vec<HolderControl>>,
    }

    impl HolderFrameSink for RecordingSink {
        fn input(&self, bytes: &[u8]) -> Result<(), String> {
            self.input.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn control(&self, op: HolderControl, _budget: Duration) -> Result<(), String> {
            self.controls.lock().unwrap().push(op);
            Ok(())
        }
    }

    fn recorded_pane() -> (Arc<RecordingSink>, DaemonPaneIo) {
        let sink = Arc::new(RecordingSink::default());
        let pane = DaemonPaneIo {
            pane_id: PaneId::new("t-1").unwrap(),
            holder_pid: 100,
            child_pid: 101,
            out: Arc::new(PaneOutput::new(
                "t-1",
                Vec::new(),
                0,
                holder_lost_output_marker,
            )),
            sink: sink.clone(),
            killed: AtomicBool::new(false),
            detached: Arc::new(AtomicBool::new(false)),
            detach: Mutex::new(DetachOutcome::NotAttempted),
            cols: AtomicU16::new(80),
            rows: AtomicU16::new(24),
            unprotected: None,
        };
        (sink, pane)
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

    /// Every control call maps to its frame; `release` without `kill` is a
    /// detach (sent once) that settles `wait` with the detach code.
    #[test]
    fn pty_holder_control_calls_map_to_holder_frames() {
        let (sink, pane) = recorded_pane();
        let mut w = pane.writer().unwrap();
        w.write_all(b"ls\r").unwrap();
        pane.resize(132, 50).unwrap();
        pane.set_paused(true).unwrap();
        pane.set_paused(false).unwrap();
        pane.release(Duration::from_millis(10)).unwrap();
        pane.release(Duration::from_millis(10)).unwrap();
        assert_eq!(sink.input.lock().unwrap().as_slice(), b"ls\r");
        assert_eq!(
            *sink.controls.lock().unwrap(),
            vec![
                HolderControl::Resize {
                    cols: 132,
                    rows: 50
                },
                HolderControl::Pause,
                HolderControl::Resume,
                HolderControl::Detach,
            ]
        );
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

    /// `kill` sends `kill` and fabricates nothing; a `release` before the
    /// exit is reported detaches and settles the exit as UNKNOWN, not 0.
    #[test]
    fn pty_holder_kill_then_release_without_exit_is_unknown_not_detach() {
        let (sink, pane) = recorded_pane();
        pane.kill(Duration::from_millis(10)).unwrap();
        assert!(
            !pane.out.is_finished(),
            "kill must not settle the exit itself"
        );
        pane.release(Duration::from_millis(20)).unwrap();
        assert_eq!(
            *sink.controls.lock().unwrap(),
            vec![HolderControl::Kill, HolderControl::Detach]
        );
        let exit = pane.wait();
        assert!(exit.is_err(), "{exit:?}");
    }

    /// The exit the holder reports after a kill wins over the release.
    #[test]
    fn pty_holder_kill_then_reported_exit_settles_with_it() {
        let (sink, pane) = recorded_pane();
        pane.kill(Duration::from_millis(10)).unwrap();
        pane.out.settle(exit_result(ExitReply {
            code: None,
            signal: Some(15),
        }));
        pane.release(Duration::from_millis(10)).unwrap();
        assert_eq!(*sink.controls.lock().unwrap(), vec![HolderControl::Kill]);
        assert_eq!(pane.wait(), Ok(SIGNAL_EXIT_CODE));
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
    }
}
