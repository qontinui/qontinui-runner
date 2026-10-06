//! `PaneIo` — the byte-source seam beneath a terminal pane.
//!
//! Everything downstream of `TerminalSession::spawn`'s reader thread consumes
//! `&[u8]` only — grid, scrollback ring, visibility tiering, the emission gate,
//! auto-response, the transcript watcher. The one place the runner assumed
//! those bytes come from a *local PTY* was the spawn function itself, which
//! held the `portable_pty` triple (reader / writer / master) plus the child it
//! waits on and kills. This module names that assumption once, behind a trait,
//! so a pane whose bytes arrive from another machine can slot in later without
//! the session layer learning a second byte source.
//!
//! [`LocalPty`] is the only implementation. Phase 1 of plan
//! `2026-08-31-remote-session-tabs-in-runner-terminal` is deliberately a
//! zero-behaviour-change refactor: the existing Rust suite is the regression
//! gate, and every error string a caller could observe is the one the inline
//! code produced before.
//!
//! # What the trait carries on purpose
//!
//! Two members are not obvious from the local case. The plan's vet added them
//! because a later `Remote` impl cannot add them without widening the interface
//! every intervening phase was built on:
//!
//! - [`PaneIo::set_paused`] — the backpressure affordance. The runner's flow
//!   control gates *emission* and never pauses reads (`EmissionGate` in
//!   `session.rs`, plan `2026-07-22-runner-pty-flow-control-emission-gating`);
//!   a local PTY needs nothing more, so [`LocalPty`] treats it as a no-op. A
//!   remote pane adds a second hop with a buffer the local gate cannot see, and
//!   this is the member that lets the gate reach across it. It is NOT a licence
//!   to import tmux-style source pausing — one regime, one invariant.
//! - [`PaneIo::credential_scrub`] — the credential-scrub obligation, made
//!   explicit. See below.
//!
//! # The credential-scrub obligation
//!
//! The PTY seam strips [`super::CREDENTIAL_VALUE_ENV_VARS`] out of the child
//! environment via [`super::scrub_credential_env_pty`], which is bound to
//! `portable_pty::CommandBuilder`. A remote pane builds no `CommandBuilder`, so
//! it inherits no scrub by construction — and the environment it would need to
//! scrub lives on another machine. The trait therefore carries the obligation in
//! two forms:
//!
//! 1. **By type, for the local impl.** [`LocalPty`] can only be spawned from a
//!    [`ScrubbedCommand`], and the ONLY constructor of that type runs the scrub.
//!    No spawn path can reach `spawn_command` with an unscrubbed builder — even
//!    one that skipped `TerminalSession::finalize_child_env`, which remains the
//!    production env tail and keeps its own (earlier) call to the same scrub.
//! 2. **By declaration, for every impl.** [`PaneIo::credential_scrub`] has no
//!    default, so an implementation must state how it discharged the
//!    obligation, and a reviewer can read the answer off the type.
//!
//! Both forms read the ONE name list in `terminal/mod.rs`; nothing here restates
//! it.
//!
//! # Output is a channel, not a `Read`
//!
//! [`PaneIo::output`] hands the session's reader thread a
//! [`mpsc::Receiver`] of byte chunks rather than a blocking `Read`. The reader
//! thread holds a DEC 2026 sync frame for at most `SYNC_FLUSH_TIME_CAP`
//! (`session.rs`), and a blocking `read()` gives it no way to wake at that
//! deadline when the pane goes quiet mid-frame — the held frame would wait for
//! the next byte. A receiver lets the ONE reader thread `recv_timeout` until
//! the deadline, so emission order (and the stream offsets stamped on it) stays
//! single-threaded by construction (plan
//! `2026-09-30-held-sync-frame-cannot-flush-during-a-blocked-pty-read`).
//! Out-of-process panes already produced into a channel and now hand it over
//! directly; [`LocalPty`] runs a pump thread that turns its blocking PTY reads
//! into chunks.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use tracing::debug;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair, PtySize};

/// How a [`PaneIo`] implementation discharged the credential-scrub obligation
/// described in the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialScrub {
    /// The child's environment was assembled in THIS process and passed
    /// through [`super::scrub_credential_env_pty`] before the child was
    /// spawned — witnessed by [`ScrubbedCommand`].
    InProcessEnv,
    /// The implementation launches no child and hands no environment to
    /// anything. Only an in-memory double can honestly answer this.
    NoChildEnv,
    /// The child's environment was assembled in THIS process and passed
    /// through [`super::scrub_credential_env_pty`] — witnessed by
    /// [`ScrubbedCommand`] — and then shipped, complete, to an OUT-OF-PROCESS
    /// PTY holder that spawns the child with exactly that environment and
    /// nothing else (`qontinui-pty-holder` clears its own environment first).
    /// [`ScrubbedCommand::to_holder_spec`] is the only constructor of a holder
    /// spec from a runner command, so the proof travels in the type (plan
    /// `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
    /// D6 as resolved 2026-09-27). `DaemonPaneIo` answers this.
    ScrubbedOutOfProcess,
}

/// A byte source and sink for one terminal pane.
///
/// Constructed once, inside `TerminalSession::spawn`, and shared with the
/// reader and waiter threads. Every method takes `&self` because the
/// implementation owns whatever locking its handles need — the session layer
/// never sees a PTY master, a child handle, or a socket.
///
/// The `String` error type is the session layer's own, so the seam adds no
/// conversion at the call sites it replaced.
pub trait PaneIo: Send + Sync {
    /// The pane's output, as a channel of byte chunks in stream order. Called
    /// once per session; the reader thread owns the receiver for the session's
    /// life. The receiver DISCONNECTS (every sender dropped) when the output
    /// ends — the pane exited, or [`Self::release`] closed it — which is the
    /// reader thread's EOF. See the module docs for why this is a channel.
    fn output(&self) -> Result<mpsc::Receiver<Vec<u8>>, String>;

    /// A writer into the pane's input. Called once per session.
    fn writer(&self) -> Result<Box<dyn Write + Send>, String>;

    /// Tell the source its viewport is now `cols` × `rows`.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String>;

    /// Block until the pane's process ends and return its exit code.
    ///
    /// Called from the waiter thread, at most once. Returns
    /// `portable_pty::ExitStatus::exit_code()` verbatim, widened to `i32`
    /// (plan `2026-08-27-operator-touch-observation-runner-emitter` §2b). That
    /// recovers genuine non-zero shell exit codes — `127` command-not-found,
    /// `126` not-executable, `2` misuse, an explicit `exit N` — which a prior
    /// `success()`-only mapping flattened to a bare `0`/`1`.
    ///
    /// ⚠️ This does **not** distinguish a crash from a signal: `ExitStatus`
    /// offers no typed signal accessor (only `success()`/`exit_code()`; the
    /// signal name surfaces solely through its `Display` text), and on
    /// unix `From<std::process::ExitStatus>` maps a signalled process through
    /// `status.code().unwrap_or(1)` — SIGKILL and SIGTERM both arrive as
    /// `exit_code() == 1`, identically to an ordinary `exit 1`. A caller may
    /// read a non-zero code as "genuine non-zero exit, cause otherwise
    /// unknown" and no richer than that; no field derived from it may imply
    /// signal attribution.
    fn wait(&self) -> Result<i32, String>;

    /// Terminate the pane's process tree, spending at most `budget` on any
    /// blocking step. `Err` means the kill could not be confirmed inside the
    /// budget — the caller logs it and carries on with teardown.
    fn kill(&self, budget: Duration) -> Result<(), String>;

    /// Backpressure affordance: ask the source to hold (`true`) or resume
    /// (`false`) production. See the module docs for why a local PTY is a
    /// no-op here.
    fn set_paused(&self, paused: bool) -> Result<(), String>;

    /// The OS pid of the pane's process, when there is one in THIS process's
    /// pid namespace.
    fn pid(&self) -> Option<u32>;

    /// The pid the runner's own crash-safety reaping (the Windows
    /// `KILL_ON_JOB_CLOSE` Job Object, `TerminalSession::spawn_with_io`) may
    /// enroll — by default [`Self::pid`]. A pane whose child is owned by an
    /// out-of-process holder answers `None`: enrolling it would end the child
    /// exactly when the runner exits, the event the holder exists to survive,
    /// and reaping is the holder's job (plan
    /// `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`, D8).
    fn job_enroll_pid(&self) -> Option<u32> {
        self.pid()
    }

    /// Whether `WireFlow` should pause this source while NO pane renders the
    /// terminal (the `Unwatched` tier). True by default — a remote pane's
    /// state is tracked on its target, so nothing here needs the bytes. A pane
    /// whose state tracking (grid, auto-response, needs-input) happens in THIS
    /// runner answers `false`: pausing it would starve those readers. Only
    /// the emission gate's backpressure is projected onto such a source.
    fn unwatched_pauses_source(&self) -> bool {
        true
    }

    /// How this implementation discharged the credential-scrub obligation.
    fn credential_scrub(&self) -> CredentialScrub;

    /// Close the underlying handles so the [`Self::output`] receiver
    /// disconnects and a reader thread blocked on it unblocks.
    /// Bounded by `budget`; `Err` means the handles could not be reached in
    /// time and will be released by process exit instead.
    fn release(&self, budget: Duration) -> Result<(), String>;
}

/// A `CommandBuilder` that has been through [`super::scrub_credential_env_pty`].
///
/// The only way to obtain one is [`ScrubbedCommand::seal`], which runs the
/// scrub — so [`LocalPty::spawn`], which takes only this type, cannot be handed
/// an unscrubbed environment. `env_remove` is idempotent, so a builder that
/// `finalize_child_env` already scrubbed is unchanged by sealing.
pub struct ScrubbedCommand(CommandBuilder);

impl ScrubbedCommand {
    /// Run the credential scrub and witness it in the type.
    pub fn seal(mut cmd: CommandBuilder) -> Self {
        super::scrub_credential_env_pty(&mut cmd);
        Self(cmd)
    }

    /// The sealed builder, for assertions.
    #[cfg(test)]
    pub(crate) fn as_command(&self) -> &CommandBuilder {
        &self.0
    }

    /// The child spec an out-of-process PTY holder runs this command from
    /// (plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
    /// D6 as resolved 2026-09-27: "the holder's spawn path builds its child
    /// through `ScrubbedCommand::seal` — the proof travels in the type"). This
    /// is the ONLY constructor of a holder spec from a runner command, so a
    /// holder child's environment is always one that went through the scrub;
    /// the holder clears its own environment and sets exactly these pairs.
    ///
    /// argv and cwd are copied as OS strings. The environment is copied
    /// through `CommandBuilder::iter_full_env_as_str`, the builder's only
    /// whole-environment iterator, which SKIPS any variable whose name or
    /// value is not valid UTF-8 — such a variable does not reach a holder pane
    /// (it does reach a `LocalPty` one). Ring size and exit linger are left at
    /// the holder's defaults.
    pub(crate) fn to_holder_spec(
        &self,
        cols: u16,
        rows: u16,
    ) -> qontinui_pty_holder::spec::ChildSpec {
        let b = &self.0;
        let mut spec = qontinui_pty_holder::spec::ChildSpec::new(if b.is_default_prog() {
            Vec::new()
        } else {
            b.get_argv().clone()
        });
        spec.cwd = b.get_cwd().cloned();
        spec.env = b
            .iter_full_env_as_str()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        spec.cols = cols.max(1);
        spec.rows = rows.max(1);
        spec
    }
}

/// A PTY pair that has been opened but not yet given a child.
///
/// Two steps rather than one because `TerminalSession::spawn` opens the PTY
/// BEFORE it assembles the child environment (identity seam, install
/// intercept, account pin), so an `openpty` failure leaves none of that
/// side-effecting work behind. Keeping the order keeps the behaviour.
pub struct OpenedPty {
    label: String,
    pair: PtyPair,
}

impl OpenedPty {
    /// Spawn `cmd` on the slave side and hand back the live pane.
    pub fn spawn(self, cmd: ScrubbedCommand) -> Result<LocalPty, String> {
        let OpenedPty { label, pair } = self;
        let child = pair
            .slave
            .spawn_command(cmd.0)
            .map_err(|e| format!("Failed to spawn shell: {}", e))?;
        let pid = child.process_id();
        // `pair.slave` drops here, after the child holds its own copy of the
        // slave side, so the master sees EOF once the child exits.
        Ok(LocalPty {
            label,
            pid,
            master: Mutex::new(Some(pair.master)),
            child: Mutex::new(Some(child)),
            preamble: Vec::new(),
            output_taken: AtomicBool::new(false),
        })
    }
}

/// The largest piece of pane output the session's reader thread processes in
/// one step, and the size of one blocking PTY read in the [`LocalPty`] pump —
/// the reader thread's read buffer before output became a channel. A channel
/// pane can queue far larger chunks (a remote seed ring is up to the whole
/// scrollback), and the reader splits those into pieces of this size so the
/// grid lock, the emission gate and each `terminal-output` event see the same
/// grain a local PTY produces.
pub(crate) const READER_CHUNK: usize = 8192;

/// Chunks queued between a [`LocalPty`]'s pump and the session's reader
/// thread: at most 32 × 8 KiB = 256 KiB in flight. BOUNDED on purpose — once
/// that much is queued, a reader thread that falls behind makes the pump block
/// in `send`, so any further backlog stays in the kernel's PTY buffer and the
/// child blocks on its own writes, as it did when the reader thread read the
/// PTY itself. An unbounded channel would instead let a flooding child grow
/// this process's memory without limit.
const LOCAL_OUTPUT_QUEUE_CHUNKS: usize = 32;

/// Turn a blocking `reader` into chunks on `tx` until it ends: `Ok(0)` (EOF),
/// a read error (how a Windows PTY reports its child's exit), or a receiver
/// that is gone. Returning drops `tx`, which disconnects the receiver — the
/// reader thread's EOF. `label` attributes the log lines only.
fn pump(mut reader: impl Read, tx: mpsc::SyncSender<Vec<u8>>, label: &str) {
    let mut buf = vec![0u8; READER_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                debug!(terminal_id = %label, "PTY reader got EOF");
                return;
            }
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    debug!(terminal_id = %label, "PTY pump: the reader thread is gone");
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                // On Windows, the PTY reader returns an error when the child exits.
                debug!(terminal_id = %label, error = %e, "PTY read error (likely process exit)");
                return;
            }
        }
    }
}

/// The local `portable_pty` implementation of [`PaneIo`].
pub struct LocalPty {
    /// Terminal id, for tracing fields only.
    label: String,
    pid: Option<u32>,
    /// `None` once [`PaneIo::release`] has dropped the OS handle.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// `None` once the waiter thread has taken it via [`PaneIo::wait`].
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    /// Bytes [`PaneIo::output`] yields before the PTY's first byte — an
    /// in-band notice (see [`Self::with_output_preamble`]). Usually empty.
    preamble: Vec<u8>,
    /// Set once [`PaneIo::output`] has started the pump: a second pump would
    /// split one PTY stream between two receivers.
    output_taken: AtomicBool,
}

impl LocalPty {
    /// Open a PTY of the given size. `label` is the terminal id, used only to
    /// attribute log lines.
    pub fn open(label: &str, cols: u16, rows: u16) -> Result<OpenedPty, String> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;
        Ok(OpenedPty {
            label: label.to_string(),
            pair,
        })
    }

    /// Have [`PaneIo::output`] yield `preamble` before the PTY's first byte —
    /// how a pane that fell back to an in-process PTY says so in-band.
    pub fn with_output_preamble(mut self, preamble: Vec<u8>) -> Self {
        self.preamble = preamble;
        self
    }
}

impl PaneIo for LocalPty {
    /// Spawns the `terminal-pump-{id}` thread, which owns a clone of the PTY
    /// reader for the session's life (see [`pump`] for how it ends and
    /// [`LOCAL_OUTPUT_QUEUE_CHUNKS`] for why the channel is bounded).
    /// [`PaneIo::release`] dropping the master is what unblocks a pump parked
    /// in `read()`, as it unblocked the reader thread before.
    ///
    /// One-shot, like every other pane's output: a second call is an `Err`.
    fn output(&self) -> Result<mpsc::Receiver<Vec<u8>>, String> {
        // The master lock is held to the end, so two racing calls cannot both
        // pass the `output_taken` check.
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        if self.output_taken.load(Ordering::Acquire) {
            return Err("PTY output already taken".to_string());
        }
        let reader = match master.as_ref() {
            Some(m) => m
                .try_clone_reader()
                .map_err(|e| format!("Failed to clone PTY reader: {}", e))?,
            None => return Err("PTY master already released".to_string()),
        };
        let (tx, rx) = mpsc::sync_channel(LOCAL_OUTPUT_QUEUE_CHUNKS);
        if !self.preamble.is_empty() {
            // The receiver is held right here and the queue is empty, so this
            // neither blocks nor fails.
            let _ = tx.send(self.preamble.clone());
        }
        let label = self.label.clone();
        std::thread::Builder::new()
            .name(format!("terminal-pump-{}", label))
            .spawn(move || pump(reader, tx, &label))
            .map_err(|e| format!("Failed to spawn PTY pump thread: {}", e))?;
        self.output_taken.store(true, Ordering::Release);
        Ok(rx)
    }

    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        match master.as_ref() {
            Some(m) => m
                .take_writer()
                .map_err(|e| format!("Failed to take PTY writer: {}", e)),
            None => Err("PTY master already released".to_string()),
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        // A released master resizes to nothing, successfully — the same
        // answer the old no-op placeholder gave after close.
        let Some(m) = master.as_ref() else {
            return Ok(());
        };
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to resize PTY: {}", e))
    }

    fn wait(&self) -> Result<i32, String> {
        // Take the child OUT of the lock before blocking on it, so a
        // concurrent `pid()`/`kill()` never queues behind a wait that lasts
        // the session's whole life.
        let child = self
            .child
            .lock()
            .map_err(|e| format!("Child lock poisoned: {}", e))?
            .take();
        let Some(mut child) = child else {
            return Err("child already waited on".to_string());
        };
        let status = child.wait().map_err(|e| e.to_string())?;
        // Recover the real code (plan
        // 2026-08-27-operator-touch-observation-runner-emitter §2b) instead of
        // flattening every non-zero exit to a bare `1` — see the trait doc for
        // what this can and cannot distinguish.
        Ok(status.exit_code() as i32)
    }

    fn kill(&self, budget: Duration) -> Result<(), String> {
        let Some(pid) = self.pid else {
            return Ok(());
        };
        // `/T` is CORRECT here: this is the terminal's OWN shell and whatever
        // it spawned, and leaving that tree behind is precisely the process
        // leak this call exists to prevent. It is categorically different from
        // `/T` on the runner's own PID.
        #[cfg(target_os = "windows")]
        {
            let mut cmd = crate::process_helpers::no_window("taskkill");
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
            match crate::drain::output_with_timeout(cmd, budget) {
                Ok(Some(_)) => Ok(()),
                Ok(None) => Err(format!(
                    "taskkill of pid {pid} exceeded its {budget:?} budget — abandoned"
                )),
                Err(e) => Err(format!("taskkill of pid {pid} could not be spawned: {e}")),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = budget;
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            Ok(())
        }
    }

    fn set_paused(&self, _paused: bool) -> Result<(), String> {
        // Local flow control gates emission and never pauses reads; there is
        // nothing upstream of a local PTY to hold.
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::InProcessEnv
    }

    fn release(&self, budget: Duration) -> Result<(), String> {
        // Dropping the master closes the OS pipe and unblocks the pump thread
        // stuck in a blocking `read()`, which then disconnects the output.
        // Bounded: a lock held by a thread blocked on a full PTY must not park
        // a shutdown past its slice.
        match crate::safe_lock::lock_with_deadline(&self.master, "terminal master pty", budget) {
            Some(mut master) => {
                drop(master.take());
                Ok(())
            }
            None => Err(format!(
                "Could not acquire the master-PTY lock for {} within the shutdown budget",
                self.label
            )),
        }
    }
}

/// An inert [`PaneIo`] for session fixtures that never spawn threads: the
/// output is already ended, the writer is a sink, everything else succeeds.
#[cfg(test)]
pub(crate) struct InertPaneIo;

/// A receiver whose output is `bytes` (one chunk, when non-empty) and then
/// immediate EOF — every sender is already dropped. For test doubles.
#[cfg(test)]
pub(crate) fn ended_output(bytes: &[u8]) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if !bytes.is_empty() {
        let _ = tx.send(bytes.to_vec());
    }
    rx
}

#[cfg(test)]
impl PaneIo for InertPaneIo {
    fn output(&self) -> Result<mpsc::Receiver<Vec<u8>>, String> {
        Ok(ended_output(b""))
    }
    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        Ok(Box::new(std::io::sink()))
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    fn wait(&self) -> Result<i32, String> {
        Ok(0)
    }
    fn kill(&self, _budget: Duration) -> Result<(), String> {
        Ok(())
    }
    fn set_paused(&self, _paused: bool) -> Result<(), String> {
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        None
    }
    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::NoChildEnv
    }
    fn release(&self, _budget: Duration) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A scripted, in-memory [`PaneIo`]: output is a fixed byte script, input
    /// lands in a shared buffer, and every control call is recorded. Proves
    /// the seam is complete — a consumer written against the trait needs no
    /// PTY to drive a full read → write → resize → pause → wait → release
    /// lifecycle.
    struct ScriptedPaneIo {
        script: Vec<u8>,
        input: Arc<Mutex<Vec<u8>>>,
        resizes: Mutex<Vec<(u16, u16)>>,
        paused: AtomicBool,
        pause_calls: AtomicUsize,
        exit_code: i32,
        killed: AtomicBool,
        released: AtomicBool,
    }

    impl ScriptedPaneIo {
        fn new(script: &[u8], exit_code: i32) -> Self {
            Self {
                script: script.to_vec(),
                input: Arc::new(Mutex::new(Vec::new())),
                resizes: Mutex::new(Vec::new()),
                paused: AtomicBool::new(false),
                pause_calls: AtomicUsize::new(0),
                exit_code,
                killed: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }
        }
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl PaneIo for ScriptedPaneIo {
        fn output(&self) -> Result<mpsc::Receiver<Vec<u8>>, String> {
            Ok(ended_output(&self.script))
        }
        fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
            Ok(Box::new(SharedWriter(self.input.clone())))
        }
        fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
            self.resizes.lock().unwrap().push((cols, rows));
            Ok(())
        }
        fn wait(&self) -> Result<i32, String> {
            Ok(self.exit_code)
        }
        fn kill(&self, _budget: Duration) -> Result<(), String> {
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn set_paused(&self, paused: bool) -> Result<(), String> {
            self.pause_calls.fetch_add(1, Ordering::SeqCst);
            self.paused.store(paused, Ordering::SeqCst);
            Ok(())
        }
        fn pid(&self) -> Option<u32> {
            None
        }
        fn credential_scrub(&self) -> CredentialScrub {
            CredentialScrub::NoChildEnv
        }
        fn release(&self, _budget: Duration) -> Result<(), String> {
            self.released.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The whole lifecycle a session drives, through `dyn PaneIo` alone.
    #[test]
    fn scripted_pane_drives_the_full_lifecycle_without_a_pty() {
        let pane: Arc<dyn PaneIo> = Arc::new(ScriptedPaneIo::new(b"hello from the pane\r\n", 7));

        // Read: the reader thread's loop shape, to EOF (disconnect).
        let rx = pane.output().expect("output");
        let mut out = Vec::new();
        while let Ok(chunk) = rx.recv() {
            out.extend_from_slice(&chunk);
        }
        assert_eq!(out, b"hello from the pane\r\n");

        // Write + flush: the input path.
        let mut writer = pane.writer().expect("writer");
        writer.write_all(b"ls\r").expect("write");
        writer.flush().expect("flush");

        pane.resize(120, 40).expect("resize");
        pane.set_paused(true).expect("pause");
        pane.set_paused(false).expect("resume");
        assert_eq!(pane.wait().expect("wait"), 7);
        pane.kill(Duration::from_millis(1)).expect("kill");
        pane.release(Duration::from_millis(1)).expect("release");
        assert_eq!(pane.pid(), None);
        assert_eq!(pane.credential_scrub(), CredentialScrub::NoChildEnv);
    }

    /// Same lifecycle, but asserting on the double's own record — the calls
    /// really reached the implementation rather than being absorbed by a
    /// default.
    #[test]
    fn scripted_pane_records_every_control_call() {
        let pane = ScriptedPaneIo::new(b"", 0);

        {
            let mut writer = pane.writer().expect("writer");
            writer.write_all(b"typed\r").expect("write");
        }
        pane.resize(80, 24).expect("resize");
        pane.resize(132, 50).expect("resize");
        pane.set_paused(true).expect("pause");
        assert!(pane.paused.load(Ordering::SeqCst));
        pane.set_paused(false).expect("resume");
        assert!(!pane.paused.load(Ordering::SeqCst));
        pane.kill(Duration::ZERO).expect("kill");
        pane.release(Duration::ZERO).expect("release");

        assert_eq!(pane.input.lock().unwrap().as_slice(), b"typed\r");
        assert_eq!(*pane.resizes.lock().unwrap(), vec![(80, 24), (132, 50)]);
        assert_eq!(pane.pause_calls.load(Ordering::SeqCst), 2);
        assert!(pane.killed.load(Ordering::SeqCst));
        assert!(pane.released.load(Ordering::SeqCst));
    }

    /// The credential scrub is discharged by the ONLY constructor of
    /// [`ScrubbedCommand`], against the one shared name list — seeded first so
    /// the assertion cannot pass vacuously (see `assert_credentials_scrubbed_pty`).
    #[test]
    fn sealing_a_command_scrubs_every_credential_value() {
        let mut cmd = CommandBuilder::new("dummy");
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        cmd.env("KEEP_ME", "yes");

        let sealed = ScrubbedCommand::seal(cmd);

        crate::terminal::assert_credentials_scrubbed_pty(
            sealed.as_command(),
            "pane_io::ScrubbedCommand::seal",
        );
        assert_eq!(
            sealed
                .as_command()
                .get_env("KEEP_ME")
                .and_then(|v| v.to_str()),
            Some("yes"),
            "the seal removes credentials and nothing else"
        );
    }

    /// Plan 2026-09-12 D6: a holder spec built from a sealed command carries
    /// the scrubbed environment — no credential value reaches an
    /// out-of-process pane — plus argv, cwd and the size.
    #[test]
    fn pty_holder_spec_from_a_sealed_command_is_scrubbed() {
        let mut cmd = CommandBuilder::new("claude");
        cmd.arg("--resume");
        cmd.cwd("/work/tree");
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        cmd.env("KEEP_ME", "yes");
        let spec = ScrubbedCommand::seal(cmd).to_holder_spec(120, 0);

        assert_eq!(
            spec.argv,
            vec![std::ffi::OsString::from("claude"), "--resume".into()]
        );
        assert_eq!(spec.cwd, Some("/work/tree".into()));
        assert_eq!((spec.cols, spec.rows), (120, 1), "a zero size is clamped");
        for (k, v) in &spec.env {
            assert!(
                !crate::terminal::CREDENTIAL_VALUE_ENV_VARS
                    .iter()
                    .any(|n| k == *n),
                "credential {k:?} reached the holder spec"
            );
            assert_ne!(v, "hunter2");
        }
        assert!(spec.env.iter().any(|(k, v)| k == "KEEP_ME" && v == "yes"));
    }

    /// The inert double answers the way the old `NoopMaster` placeholder did:
    /// a resize after release still succeeds, and there is nothing to read.
    #[test]
    fn inert_pane_is_a_faithful_noop() {
        let pane = InertPaneIo;
        pane.release(Duration::ZERO).expect("release");
        pane.resize(100, 40).expect("resize after release");
        let rx = pane.output().expect("output");
        assert_eq!(rx.recv(), Err(mpsc::RecvError), "already ended");
        assert_eq!(pane.credential_scrub(), CredentialScrub::NoChildEnv);
    }

    /// The pump forwards every byte in order and then disconnects the
    /// receiver at EOF — the reader thread's end-of-output.
    #[test]
    fn pump_forwards_a_reader_then_disconnects_at_eof() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let (tx, rx) = mpsc::sync_channel(LOCAL_OUTPUT_QUEUE_CHUNKS);
        let src = data.clone();
        let t = std::thread::spawn(move || pump(std::io::Cursor::new(src), tx, "pump-eof"));
        let got: Vec<u8> = rx.iter().flatten().collect();
        t.join().unwrap();
        assert_eq!(got, data);
    }

    /// A reader that yields some bytes and then fails — how a Windows PTY
    /// reports its child's exit.
    struct FailsAfter(Option<Vec<u8>>);

    impl Read for FailsAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.take() {
                Some(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                None => Err(std::io::Error::other("child exited")),
            }
        }
    }

    /// A read error ends the output exactly like EOF: what was read is
    /// delivered, then the receiver disconnects.
    #[test]
    fn pump_ends_the_output_on_a_read_error() {
        let (tx, rx) = mpsc::sync_channel(LOCAL_OUTPUT_QUEUE_CHUNKS);
        pump(FailsAfter(Some(b"last words".to_vec())), tx, "pump-err");
        assert_eq!(rx.recv().unwrap(), b"last words");
        assert_eq!(rx.recv(), Err(mpsc::RecvError));
    }

    /// A pump whose receiver is gone stops instead of reading forever.
    #[test]
    fn pump_stops_when_the_receiver_is_gone() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        // An endless source: only the dropped receiver can end this call.
        pump(std::io::repeat(b'x'), tx, "pump-gone");
    }

    /// A pump facing a full channel blocks rather than buffering: the queue
    /// never holds more than its capacity.
    #[test]
    fn pump_blocks_on_a_full_channel_instead_of_growing() {
        let (tx, rx) = mpsc::sync_channel(2);
        let t = std::thread::spawn(move || pump(std::io::repeat(b'x'), tx, "pump-full"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!t.is_finished(), "the pump waits on the full channel");
        // The queued chunks are whole reads.
        let bound = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(bound).unwrap().len(), READER_CHUNK);
        assert_eq!(rx.recv_timeout(bound).unwrap().len(), READER_CHUNK);
        drop(rx);
        t.join().unwrap();
    }

    /// A real local PTY through the seam: open, spawn a one-shot echo through
    /// a sealed command, read its output via `output()`, wait for exit via
    /// `wait()`, release. The same shape as `drive_real_pty_into` in
    /// `session.rs`, but with nothing but `dyn PaneIo` in the caller's hands.
    #[test]
    fn local_pty_round_trips_through_the_trait() {
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg("echo PANEIO_MARKER");
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg("echo PANEIO_MARKER");
            c
        };
        cmd.env("TERM", "xterm-256color");
        // Seed a credential so the seal is exercised on the production path,
        // not only in the unit test above.
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }

        let pane: Arc<dyn PaneIo> = Arc::new(
            LocalPty::open("paneio-test", 80, 24)
                .expect("openpty")
                .spawn(ScrubbedCommand::seal(cmd))
                .expect("spawn"),
        );
        assert_eq!(pane.credential_scrub(), CredentialScrub::InProcessEnv);
        assert!(pane.pid().is_some(), "a spawned child has a pid");

        let rx = pane.output().expect("output");
        assert_eq!(
            pane.output().err().as_deref(),
            Some("PTY output already taken"),
            "a second pump would split the stream"
        );
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink = collected.clone();
        // Own thread, like production: on ConPTY the pump's `read()` keeps
        // blocking after the child exits until the MASTER is released.
        let reader_thread = std::thread::spawn(move || {
            while let Ok(chunk) = rx.recv() {
                sink.lock().unwrap().extend_from_slice(&chunk);
            }
        });

        let code = pane.wait().expect("wait");
        assert_eq!(code, 0, "echo exits cleanly");
        assert!(
            pane.wait().is_err(),
            "a second wait has no child to wait on"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if String::from_utf8_lossy(&collected.lock().unwrap()).contains("PANEIO_MARKER") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        pane.release(Duration::from_secs(2)).expect("release");
        // Release must end the output: the pump unblocks and drops its sender,
        // so the receiving thread finishes rather than hanging.
        let joined = std::thread::spawn(move || reader_thread.join());
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !joined.is_finished() {
            assert!(
                std::time::Instant::now() < end,
                "the output did not end after release"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let text = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        assert!(
            text.contains("PANEIO_MARKER"),
            "read via the seam: {text:?}"
        );
        pane.resize(100, 40)
            .expect("resize after release is a successful no-op");
        assert!(
            pane.output().is_err(),
            "no output after the master is released"
        );
    }

    /// The §2b fix, pinned: a real non-zero shell exit code (127,
    /// command-not-found) survives through `PtyPaneIo::wait` instead of
    /// flattening to the old bare `1`. Plan
    /// `2026-08-27-operator-touch-observation-runner-emitter` §2b.
    #[test]
    fn local_pty_recovers_the_real_nonzero_exit_code() {
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg("exit 127");
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg("exit 127");
            c
        };
        cmd.env("TERM", "xterm-256color");

        let pane: Arc<dyn PaneIo> = Arc::new(
            LocalPty::open("paneio-exit-code-test", 80, 24)
                .expect("openpty")
                .spawn(ScrubbedCommand::seal(cmd))
                .expect("spawn"),
        );
        // Drain the output so a full pipe can never wedge the wait — same
        // discipline as the round-trip test above.
        let rx = pane.output().expect("output");
        let reader_thread = std::thread::spawn(move || while rx.recv().is_ok() {});

        let code = pane.wait().expect("wait");
        assert_eq!(
            code, 127,
            "a real non-zero exit code must survive, not flatten to 1"
        );
        pane.release(Duration::from_secs(2)).expect("release");
        let _ = reader_thread.join();
    }
}
