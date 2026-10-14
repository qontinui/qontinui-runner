//! The pane: the PTY a holder owns, its child, and its output ring.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2. One [`Pane`] per holder (D13). Three threads keep it:
//!
//! - **reader** — reads the PTY master into the [`OutputRing`] — and through
//!   the [`ModeTracker`] (Phase 2b) — for the whole life of the pane, ALWAYS. Nothing a client does pauses it: flow control
//!   gates what a connection is SENT, never what the holder reads (the
//!   runner's one invariant, "gate emission, never pause reads"). A child
//!   therefore never blocks on a full PTY buffer because a client is slow or
//!   absent; what the ring can no longer hold is reported to a late client as
//!   `output_lost`, exactly.
//! - **waiter** — waits for the child to exit and settles its [`ExitReply`]:
//!   after the exit it gives the reader up to [`EXIT_DRAIN_GRACE`] to reach
//!   EOF, so the child's last output lands in the ring BEFORE the exit is
//!   visible. (A grandchild still holding the PTY open keeps EOF away; the
//!   grace bounds that.) On Unix it waits with `waitid(WNOWAIT)` first and
//!   reaps only under the state lock, so [`Pane::kill`] can never signal a pid
//!   that was reaped and recycled.
//! - **writer** — the ONLY thread that writes the PTY master. Input from
//!   every connection is queued to it ([`INPUT_QUEUE_FRAMES`] frames, bounded),
//!   so a child that is not reading its stdin blocks THIS thread and never a
//!   connection's dispatch: `kill`, `resize`, `pause` and `detach` keep being
//!   served while a big paste waits for the child. Only once the queue itself
//!   is full does the dispatch wait (backpressure to the runner); a `kill` on
//!   any connection still lands, and once the child is gone the writer
//!   DISCARDS what is queued, which unblocks everything behind it.
//! - the per-connection **pumps** in `server` read the ring; they live there.
//!
//! **When the holder exits** ([`Pane::wait_for_delivery`], called by `main`): never
//! because a client went away (D13: "it never exits because the runner went
//! away"). Only once the child has exited AND either (a) an `exit` frame was
//! delivered to some attached client, or (b) [`crate::spec::ChildSpec::exit_linger`]
//! (default 10 minutes) passed with nobody collecting it. (b) is the bound for
//! a pane whose runner is gone for good: the holder then leaves, its lock is
//! released, and a census sees the pane `Dead`. A runner that comes back
//! within the linger re-attaches and gets the tail and the exit code.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::modes::ModeTracker;
use crate::protocol::ExitReply;
use crate::ring::OutputRing;
use crate::spec::ChildSpec;

/// How long after the child's exit the waiter lets the reader drain the PTY
/// before it settles the exit without EOF.
pub const EXIT_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// `kill`: how long after `SIGHUP` before `SIGKILL` (Unix).
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// One PTY read.
const READ_CHUNK: usize = 16 * 1024;

/// Input frames queued to the PTY writer thread before a dispatch thread
/// waits. The runner sends input in chunks of at most 64 KiB, so this is a few
/// MiB of paste in flight, which no interactive use reaches.
pub const INPUT_QUEUE_FRAMES: usize = 64;

/// How often a dispatch waiting on a full input queue re-tries it.
const INPUT_QUEUE_POLL: Duration = Duration::from_millis(10);

/// Everything the threads share, under one lock with one condvar.
#[derive(Debug)]
pub struct PaneState {
    pub ring: OutputRing,
    /// Every byte the ring was ever given, parsed for terminal modes (plan
    /// D14). Fed under this lock with the ring, so its state is the state at
    /// the ring's `end_offset` and its preamble log reaches back to the ring's
    /// `start_offset`.
    pub modes: ModeTracker,
    /// The PTY master read hit EOF (or an error, which ends it the same way).
    pub eof: bool,
    /// The child has exited (it may not be reaped yet). Unix: set under this
    /// lock, after `waitid(WNOWAIT)` and before the reap.
    pub child_gone: bool,
    /// Settled once the child is gone AND its output is in the ring (or the
    /// drain grace ran out). `exit` frames are sent only after this.
    pub exit: Option<ExitReply>,
    /// An `exit` frame reached a client.
    pub exit_delivered: bool,
    /// How the child ended, fixed at the moment its exit is observed:
    /// `Some(true)` when a kill had REACHED the live child first
    /// ([`Pane::kill_now`]), `Some(false)` when it exited on its own. Set once,
    /// by the waiter; read by [`Pane::ended_by_kill`].
    pub ended_by_kill: Option<bool>,
    /// Current PTY size, (cols, rows).
    pub size: (u16, u16),
}

/// The pane a holder owns.
pub struct Pane {
    child_pid: u32,
    /// The directory the child was started in (the spec's), for restore
    /// metadata when the program never reports one with `OSC 7`.
    spawn_cwd: Option<std::ffi::OsString>,
    state: Mutex<PaneState>,
    changed: Condvar,
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Input for the writer thread (see the module docs).
    input_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// The generation of the CURRENT input connection. `open_input` on a new
    /// connection bumps it ([`Pane::supersede_input`]); every enqueue checks
    /// its own generation against it UNDER this lock, so once a newer input
    /// connection exists an older one can enqueue nothing more — its bytes can
    /// never land after (or interleave with) the newer connection's.
    input_gen: Mutex<u64>,
    #[cfg(windows)]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// Windows: a holder-owned `KILL_ON_JOB_CLOSE` job holding the child, so
    /// `kill` ends the child's whole TREE (`TerminateJobObject`), not the child
    /// alone. `None` when the child could not be assigned (then `kill` falls
    /// back to terminating the child only, and says so in the holder log).
    #[cfg(windows)]
    job: Option<win_job::Job>,
    exit_linger: Duration,
    /// The child's start token (`crate::proc_identity`), taken in
    /// [`Pane::spawn`] BEFORE the waiter can reap it — so it is the child's,
    /// never a recycled pid's.
    child_start: Option<String>,
    /// Unix: what the waiter's session reap did (`crate::reap`, the kill
    /// path), kept for [`Pane::reap_tree`] to report.
    #[cfg(unix)]
    session_reap: Mutex<Option<crate::reap::ReapReport>>,
    /// A kill was sent while the child HAD NOT EXITED AT THE MOMENT OF THE
    /// CHECK: set under the state lock, only while `child_gone` is false and
    /// a non-reaping `waitid(WNOHANG|WNOWAIT)` (Unix) / a zero-timeout wait
    /// (Windows) sees no exit, right before the signal / the job termination.
    /// A `kill` that arrives after the child has exited sets nothing.
    ///
    /// **Inherent gap** (review round 3, R6): a child that exits on its own in
    /// the instant between that check and the signal (or `TerminateJobObject`)
    /// is recorded as killed, and its leftovers are reaped. No OS offers an
    /// atomic "signal only if not yet exited"; the window is the few
    /// instructions between two syscalls. Only a reached kill makes the pane's end a
    /// KILL, the one end that reaps the child's leftovers (plan D8 as decided
    /// 2026-10-14): a normal exit leaves `nohup` / `disown` jobs and
    /// deliberately detached programs alone, as an in-process PTY always did.
    kill_reached: AtomicBool,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("child_pid", &self.child_pid)
            .finish_non_exhaustive()
    }
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// The child's command. The environment is CLEARED and replaced by exactly
/// `spec.env` (plan D6: nothing of the holder's own environment reaches the
/// pane).
fn build_command(spec: &ChildSpec) -> CommandBuilder {
    let mut cmd = if spec.argv.is_empty() {
        CommandBuilder::new_default_prog()
    } else {
        CommandBuilder::from_argv(spec.argv.clone())
    };
    cmd.env_clear();
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.cwd(cwd);
    }
    cmd
}

impl Pane {
    /// Open the PTY, spawn the child on it, and start the reader and waiter.
    pub fn spawn(spec: &ChildSpec) -> io::Result<Arc<Pane>> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| other(format!("openpty: {e}")))?;
        let child = pair
            .slave
            .spawn_command(build_command(spec))
            .map_err(|e| other(format!("spawn: {e}")))?;
        // The holder keeps only the master; the child holds the slave, so the
        // master sees EOF once the child (and anything it left holding the
        // slave) is gone.
        drop(pair.slave);
        let master = pair.master;
        let Some(child_pid) = child.process_id() else {
            let mut child = child;
            let _ = child.kill();
            return Err(other("child has no pid"));
        };
        let reader = master
            .try_clone_reader()
            .map_err(|e| other(format!("master reader: {e}")))?;
        let writer = master
            .take_writer()
            .map_err(|e| other(format!("master writer: {e}")))?;
        #[cfg(windows)]
        let killer = child.clone_killer();
        #[cfg(windows)]
        let job = match win_job::Job::for_child(child_pid) {
            Ok(j) => Some(j),
            Err(e) => {
                eprintln!("pty-holder: child {child_pid} not placed in a job ({e}); kill ends the child only");
                None
            }
        };
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE_FRAMES);
        // Before the waiter thread exists: the child cannot have been reaped.
        let child_start = crate::proc_identity::start_token(child_pid);

        let pane = Arc::new(Pane {
            child_pid,
            spawn_cwd: spec.cwd.clone(),
            state: Mutex::new(PaneState {
                ring: OutputRing::new(spec.ring_capacity()),
                modes: ModeTracker::new(),
                eof: false,
                child_gone: false,
                exit: None,
                exit_delivered: false,
                ended_by_kill: None,
                size: (spec.cols, spec.rows),
            }),
            changed: Condvar::new(),
            master: Mutex::new(master),
            input_tx,
            input_gen: Mutex::new(0),
            #[cfg(windows)]
            killer: Mutex::new(killer),
            #[cfg(windows)]
            job,
            exit_linger: spec.exit_linger(),
            child_start,
            #[cfg(unix)]
            session_reap: Mutex::new(None),
            kill_reached: AtomicBool::new(false),
        });

        let p = Arc::clone(&pane);
        std::thread::Builder::new()
            .name("pty-holder-reader".into())
            .spawn(move || p.read_loop(reader))?;
        let p = Arc::clone(&pane);
        std::thread::Builder::new()
            .name("pty-holder-writer".into())
            .spawn(move || p.write_loop(writer, input_rx))?;
        let p = Arc::clone(&pane);
        std::thread::Builder::new()
            .name("pty-holder-waiter".into())
            .spawn(move || p.wait_loop(child))?;
        Ok(pane)
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// The child's start token, taken before it could be reaped.
    pub fn child_start(&self) -> Option<&str> {
        self.child_start.as_deref()
    }

    /// The directory the child was started in.
    pub fn spawn_cwd(&self) -> Option<&std::ffi::OsString> {
        self.spawn_cwd.as_ref()
    }

    /// The shared state. Never hold this across I/O on a client connection.
    pub fn lock_state(&self) -> MutexGuard<'_, PaneState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Wait for any state change, at most `timeout`.
    pub fn wait_changed<'a>(
        &self,
        guard: MutexGuard<'a, PaneState>,
        timeout: Duration,
    ) -> MutexGuard<'a, PaneState> {
        match self.changed.wait_timeout(guard, timeout) {
            Ok((g, _)) => g,
            Err(p) => p.into_inner().0,
        }
    }

    /// Wake every waiter (a pump whose flow flag changed, `main`).
    pub fn notify(&self) {
        // Taking the lock orders this wake after any state change the caller
        // made, so a waiter between its check and its wait cannot miss it.
        drop(self.lock_state());
        self.changed.notify_all();
    }

    fn read_loop(&self, mut reader: Box<dyn Read + Send>) {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut st = self.lock_state();
                    st.ring.push(&buf[..n]);
                    st.modes.feed(&buf[..n]);
                    let ring_start = st.ring.start_offset();
                    st.modes.forget_before(ring_start);
                    drop(st);
                    self.changed.notify_all();
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Linux reports a hung-up PTY master as EIO, not 0: both end it.
                Err(_) => break,
            }
        }
        self.lock_state().eof = true;
        self.changed.notify_all();
    }

    /// Settle the exit once the child is gone and its output has drained.
    fn settle(&self, exit: ExitReply) {
        let deadline = Instant::now() + EXIT_DRAIN_GRACE;
        let mut st = self.lock_state();
        st.child_gone = true;
        let killed = self.kill_reached.load(Ordering::SeqCst);
        st.ended_by_kill.get_or_insert(killed);
        while !st.eof {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            st = self.wait_changed(st, deadline - now);
        }
        st.exit = Some(exit);
        drop(st);
        self.changed.notify_all();
    }

    #[cfg(unix)]
    fn wait_loop(&self, child: Box<dyn portable_pty::Child + Send + Sync>) {
        let pid = self.child_pid as libc::pid_t;
        // Phase 1: wait for the exit WITHOUT reaping, so the pid stays ours
        // (a zombie) while `kill` may still be looking at it.
        let exited = loop {
            // SAFETY: siginfo_t is plain data; zeroed is a valid initial value.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: waitid on our own child pid with a valid out-pointer.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                break Ok(());
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINTR) {
                break Err(e);
            }
        };
        // Phase 2: under the lock, mark it gone and fix HOW it ended — `kill`
        // checks `child_gone` under the same lock, so from here on it can
        // neither signal the pid nor turn this exit into a kill.
        let killed = {
            let mut st = self.lock_state();
            st.child_gone = true;
            let killed = self.kill_reached.load(Ordering::SeqCst);
            st.ended_by_kill = Some(killed);
            killed
        };
        // Plan D8, kill path only: the child was killed and is a zombie now —
        // `exited` says waitid saw it so — so its pid (= its session id) is
        // pinned: end what it left in its session BEFORE the reap below
        // releases that id. A child that exited on its own leaves its session
        // as an in-process PTY did. A waitid failure leaves the session
        // unswept, and the report says so.
        let session_reap = match (&exited, killed) {
            (_, false) => None,
            (Ok(()), true) => Some(crate::reap::reap_session_of_exited_child(self.child_pid)),
            (Err(e), true) => {
                let mut r = crate::reap::ReapReport::default();
                r.note(format!(
                    "waitid failed ({e}): the killed child is not provably a zombie, so its \
                     session was NOT swept"
                ));
                Some(r)
            }
        };
        *self.session_reap.lock().unwrap_or_else(|p| p.into_inner()) = session_reap;
        // Phase 3: reap it.
        let mut status: libc::c_int = 0;
        let reaped = {
            let _st = self.lock_state();
            loop {
                // SAFETY: reaping our own child pid; `status` is a valid out-pointer.
                let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
                if rc == pid {
                    break true;
                }
                if rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break false;
            }
        };
        // portable-pty's handle is a `std::process::Child`; dropping it never
        // waits or kills.
        drop(child);
        let exit = if !reaped {
            // Reaped by someone else (impossible for a holder, which never
            // installs a SIGCHLD reaper): the code is UNKNOWN, never 0.
            ExitReply {
                code: None,
                signal: None,
            }
        } else if libc::WIFEXITED(status) {
            ExitReply {
                code: Some(libc::WEXITSTATUS(status)),
                signal: None,
            }
        } else if libc::WIFSIGNALED(status) {
            ExitReply {
                code: None,
                signal: Some(libc::WTERMSIG(status)),
            }
        } else {
            ExitReply {
                code: None,
                signal: None,
            }
        };
        self.settle(exit);
    }

    #[cfg(windows)]
    fn wait_loop(&self, mut child: Box<dyn portable_pty::Child + Send + Sync>) {
        // Blocking by design: a dedicated thread in the holder process, which
        // has no async runtime and no thread pool to starve.
        let exit = match child.wait() {
            Ok(status) => ExitReply {
                code: Some(status.exit_code() as i32),
                signal: None,
            },
            Err(_) => ExitReply {
                code: None,
                signal: None,
            },
        };
        self.settle(exit);
    }

    /// Queue input for the child. Returns as soon as the bytes are queued;
    /// waits only while [`INPUT_QUEUE_FRAMES`] frames are already queued (the
    /// child has not read the last few MiB) — the pane's backpressure, felt by
    /// one connection's dispatch and never by the writer of a `kill` on
    /// another connection — and at most `timeout`. A wait that runs out is
    /// `TimedOut`: the caller closes that connection rather than holding its
    /// thread and its connection slot for as long as the child ignores stdin
    /// (the runner reattaches, and kills on a connection of its own).
    ///
    /// `generation` is the caller's from [`Pane::supersede_input`]. A caller
    /// that has been superseded gets `ErrorKind::Other` ("superseded") and
    /// its bytes are DISCARDED, never enqueued: the generation check and the
    /// (non-blocking) enqueue happen under one lock, so no older connection's
    /// frame can be queued after a newer connection exists.
    pub fn write_input(&self, bytes: &[u8], timeout: Duration, generation: u64) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        let mut chunk = bytes.to_vec();
        loop {
            let sent = {
                let current = self.input_gen.lock().unwrap_or_else(|p| p.into_inner());
                if *current != generation {
                    return Err(io::Error::other(
                        "superseded: a newer input connection replaced this one",
                    ));
                }
                self.input_tx.try_send(chunk)
            };
            match sent {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "pty writer is gone",
                    ))
                }
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "the child has not read its input queue within the bound",
                        ));
                    }
                    chunk = back;
                    // A full queue means the child is not reading: a short
                    // poll costs nothing it is not already waiting for.
                    std::thread::sleep(INPUT_QUEUE_POLL);
                }
            }
        }
    }

    /// The writer thread: the only writer of the PTY master. Once a write
    /// fails or the child is gone, everything still queued is DISCARDED
    /// (there is no reader for it), which is what unblocks a dispatch waiting
    /// on a full queue.
    fn write_loop(&self, mut w: Box<dyn Write + Send>, rx: std::sync::mpsc::Receiver<Vec<u8>>) {
        let mut broken = false;
        while let Ok(bytes) = rx.recv() {
            if broken || self.lock_state().child_gone {
                continue;
            }
            if w.write_all(&bytes).and_then(|()| w.flush()).is_err() {
                broken = true;
            }
        }
    }

    /// Make the caller the pane's CURRENT input connection and return its
    /// generation. Every older input connection is superseded from this
    /// instant: its pending and future input is discarded (see
    /// [`Pane::write_input`]). Bytes it had already enqueued stay queued —
    /// they precede everything the new connection sends.
    pub fn supersede_input(&self) -> u64 {
        let mut current = self.input_gen.lock().unwrap_or_else(|p| p.into_inner());
        *current = current.wrapping_add(1);
        *current
    }

    /// Resize the PTY (the child gets `SIGWINCH` on Unix).
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        if cols == 0 || rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a PTY is at least 1x1",
            ));
        }
        self.master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| other(format!("resize: {e}")))?;
        self.lock_state().size = (cols, rows);
        Ok(())
    }

    /// End the child, in the background. Unix: `SIGHUP` to the child's process
    /// group (it is a session and group leader on its PTY) and to the child,
    /// then `SIGKILL` after [`KILL_GRACE`] if it is still there. Every signal is
    /// sent under the state lock and only while `child_gone` is false, so a
    /// reaped, recycled pid is never signalled. Windows: terminate the child.
    pub fn kill(self: &Arc<Self>) {
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("pty-holder-kill".into())
            .spawn(move || me.kill_now());
        if spawned.is_err() {
            self.kill_now();
        }
    }

    #[cfg(unix)]
    fn signal_child(&self, sig: libc::c_int) -> bool {
        let st = self.lock_state();
        if st.child_gone {
            return false;
        }
        let pid = self.child_pid as libc::pid_t;
        // The child may have exited on its own an instant ago, before the
        // waiter marked it gone: then this kill reaches nothing, and must not
        // turn its normal exit into a kill (review round 2, N2). A zombie is
        // seen without reaping it (`WNOWAIT`), so the waiter's reap is intact.
        // SAFETY: siginfo_t is plain data; zeroed (si_pid 0) means "no
        // state change yet" for WNOHANG.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a non-blocking, non-reaping waitid on our own child.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        // SAFETY: reading the pid field waitid filled (or left zeroed).
        if rc == 0 && unsafe { info.si_pid() } != 0 {
            return false;
        }
        self.kill_reached.store(true, Ordering::SeqCst);
        // SAFETY: plain signals to our own, still-unreaped child and its
        // process group (pgid == pid: portable-pty `setsid()`s the child).
        unsafe {
            libc::kill(-pid, sig);
            libc::kill(pid, sig);
        }
        drop(st);
        true
    }

    /// [`Pane::kill`], on the calling thread.
    pub fn kill_now(&self) {
        #[cfg(unix)]
        {
            if !self.signal_child(libc::SIGHUP) {
                return;
            }
            let deadline = Instant::now() + KILL_GRACE;
            let mut st = self.lock_state();
            while !st.child_gone {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                st = self.wait_changed(st, deadline - now);
            }
            drop(st);
            self.signal_child(libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            {
                let _st = self.lock_state();
                // Gone, or exited an instant ago and not yet settled: this
                // kill reaches nothing (review round 2, N2).
                if _st.child_gone || win_job::has_exited(self.child_pid) {
                    return;
                }
                self.kill_reached.store(true, Ordering::SeqCst);
            }
            // The whole tree when the child is in our job; the child alone
            // otherwise.
            if let Some(job) = &self.job {
                if job.terminate().is_ok() {
                    return;
                }
            }
            let _ = self.killer.lock().unwrap_or_else(|p| p.into_inner()).kill();
        }
    }

    /// What the holder reaped of its child's tree, reported once the pane is
    /// over (plan D8; the decision of 2026-10-14 on WHEN it reaps):
    ///
    /// - **The pane was killed** ([`Pane::kill_now`] — a tab closed, or the
    ///   runner gave up on the pane). Unix: the child's session was swept by
    ///   the waiter while the child was an unreaped zombie (`crate::reap`), and
    ///   here, on the scope route, everything else in the holder's own scope.
    ///   Windows: `kill_now` already terminated the holder-owned job.
    /// - **The child exited on its own.** Nothing is reaped here: what it left
    ///   running (`nohup` / `disown` jobs, a tmux server, an agent) keeps
    ///   running, as with an in-process PTY on Linux. Windows: the holder
    ///   disarms its job's `KILL_ON_JOB_CLOSE` first
    ///   ([`Pane::release_leftovers_on_normal_exit`]), so they survive the
    ///   holder's exit too.
    pub fn reap_tree(&self) -> crate::reap::ReapReport {
        let mut report = crate::reap::ReapReport::default();
        if self.ended_by_kill() != Some(true) {
            report.note("the child exited on its own: its leftovers are not reaped");
            return report;
        }
        #[cfg(unix)]
        {
            if let Some(session) = self
                .session_reap
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                report.absorb(session);
            }
            report.absorb(crate::reap::reap_own_scope());
        }
        #[cfg(windows)]
        report.note(if self.job.is_some() {
            "killed: the job holding the child's tree was terminated"
        } else {
            "killed: the child was never in a job, so only the child was terminated"
        });
        report
    }

    /// How the child ended (`Some(true)`: a kill reached it), once its exit is
    /// settled; `None` before.
    pub fn ended_by_kill(&self) -> Option<bool> {
        self.lock_state().ended_by_kill
    }

    /// A NORMAL end (the child exited on its own): let what it left running
    /// outlive this holder. Windows: clear the job's `KILL_ON_JOB_CLOSE`, which
    /// would otherwise end every process still in it the moment the holder —
    /// the job's last handle — exits. **This differs from the old in-process
    /// PTY on Windows**, whose leftovers sat in the runner's own
    /// `KILL_ON_JOB_CLOSE` job and died when the RUNNER exited: here they live
    /// on, as on Linux, because the holder exits with its pane, far earlier
    /// than a runner, and killing them then would end a `nohup`-style program
    /// the moment its tab's command finished. A crash of the holder before
    /// this point, or a kill, keeps the flag armed (crash safety). Unix:
    /// nothing to do — no reap follows a normal exit.
    pub fn release_leftovers_on_normal_exit(&self) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            return job.disarm_kill_on_close().map_err(|e| e.to_string());
        }
        Ok(())
    }

    /// Record that an `exit` frame reached a client.
    pub fn mark_exit_delivered(&self) {
        self.lock_state().exit_delivered = true;
        self.changed.notify_all();
    }

    /// Block until the child's exit is settled and return it.
    pub fn wait_for_exit(&self) -> ExitReply {
        let mut st = self.lock_state();
        loop {
            if let Some(e) = st.exit {
                return e;
            }
            st = self.wait_changed(st, Duration::from_secs(60));
        }
    }

    /// After [`Pane::wait_for_exit`]: block until the holder may exit — the
    /// exit was delivered, or nobody collected it within the linger (see the
    /// module docs).
    pub fn wait_for_delivery(&self) {
        let mut st = self.lock_state();
        let deadline = Instant::now() + self.exit_linger;
        while !st.exit_delivered {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            st = self.wait_changed(st, deadline - now);
        }
    }
}

/// The holder's own exit status for a child exit: the child's code, `128 +
/// signal` for a signal death (the shell convention), 1 when unknown.
pub fn holder_exit_code(exit: &ExitReply) -> u8 {
    match (exit.code, exit.signal) {
        (Some(c), _) => (c & 0xFF) as u8,
        (None, Some(s)) => (128 + (s & 0x7F)) as u8,
        (None, None) => 1,
    }
}

/// Windows: the holder-owned job that holds a pane's child tree.
#[cfg(windows)]
mod win_job {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// Whether process `pid` — the pane's child, whose handle the waiter's
    /// `portable_pty` child still holds, so the pid cannot have been reused —
    /// has exited. Unreadable counts as NOT exited: the kill proceeds, as it
    /// did before this check existed.
    pub fn has_exited(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::{WaitForSingleObject, PROCESS_SYNCHRONIZE};
        // SAFETY: no pointers; a non-null handle is closed below.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if h.is_null() {
            return false;
        }
        // SAFETY: a valid handle with SYNCHRONIZE; a zero timeout only polls.
        let running = unsafe { WaitForSingleObject(h, 0) } == WAIT_TIMEOUT;
        // SAFETY: closing the handle opened above, once.
        unsafe { CloseHandle(h) };
        !running
    }

    /// An owned job handle. `KILL_ON_JOB_CLOSE`: when the holder exits (its
    /// last handle closes) whatever is still in the job ends too — the same
    /// outcome as a holder death hanging up the PTY on Unix.
    pub struct Job(HANDLE);

    // SAFETY: a job handle is a kernel object handle, usable from any thread.
    unsafe impl Send for Job {}
    // SAFETY: as above; the only operations are thread-safe Win32 calls.
    unsafe impl Sync for Job {}

    impl Job {
        /// Create the job and put `child_pid` in it. Descendants the child
        /// starts afterwards are in it too (a job is inherited). A process the
        /// child spawned before this call is not — the window is the few
        /// instructions between the spawn and this call.
        pub fn for_child(child_pid: u32) -> io::Result<Job> {
            // SAFETY: plain Win32 calls with valid (null or owned) arguments;
            // every handle is closed on every failure path or owned by `Job`.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() || job == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }
                let job = Job(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, child_pid);
                if process.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let assigned = AssignProcessToJobObject(job.0, process);
                let err = io::Error::last_os_error();
                CloseHandle(process);
                if assigned == 0 {
                    return Err(err);
                }
                Ok(job)
            }
        }

        /// Clear `KILL_ON_JOB_CLOSE`: the job's processes then outlive its
        /// last handle (`Pane::release_leftovers_on_normal_exit`).
        pub fn disarm_kill_on_close(&self) -> io::Result<()> {
            // SAFETY: an owned, valid job handle; `info` is plain data that
            // outlives the call.
            unsafe {
                let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                if SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        }

        /// End every process in the job.
        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: an owned, valid job handle.
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned and closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
