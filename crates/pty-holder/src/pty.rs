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
//! **When the holder exits** ([`Pane::wait_for_end`], called by `main`): never
//! because a client went away (D13: "it never exits because the runner went
//! away"). Only once the child has exited AND either (a) an `exit` frame was
//! delivered to some attached client, or (b) [`crate::spec::ChildSpec::exit_linger`]
//! (default 10 minutes) passed with nobody collecting it. (b) is the bound for
//! a pane whose runner is gone for good: the holder then leaves, its lock is
//! released, and a census sees the pane `Dead`. A runner that comes back
//! within the linger re-attaches and gets the tail and the exit code.
//!
//! **Holder upgrade (plan Phase 8, D12a; Unix only).** A pane can be handed to
//! a successor holder process. Three hooks here serve it (`crate::handoff`
//! drives them): the reader can be QUIESCED at an exact stream offset
//! ([`Pane::quiesce_reads`] — it parks between reads, woken through a pipe
//! rather than by closing anything) and later resumed or retired; input can be
//! FROZEN ([`Pane::freeze_input`] — queued input drains to the child first, and
//! input arriving meanwhile is held, not written, so it can travel to the
//! successor); and a successor builds an ADOPTED pane ([`Pane::adopt`]) from
//! the inherited master fd. An adopted pane's master is a plain `File` with
//! `TIOCSWINSZ` for resizes — `portable-pty` cannot be given an existing fd
//! (D12a rule 4) — and its child is not this process's own: there is no
//! `waitpid`, the exit is detected as master EOF/`EIO`, and its exit code is
//! UNKNOWN (`code: None`, D12a rule 3), never 0.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Most input a FROZEN pane holds for a holder upgrade (plan Phase 8) before a
/// dispatch waits instead: at most [`INPUT_QUEUE_FRAMES`] frames, and this many
/// bytes in all.
pub const MAX_HELD_INPUT_BYTES: usize = 1024 * 1024;

/// Whether the reader thread may read the master (plan Phase 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadGate {
    /// Normal operation: read whenever the master is readable.
    Run,
    /// A holder upgrade asked the reader to stop between reads; it parks
    /// (`PaneState::read_parked`) until the gate changes.
    Quiesce,
    /// The pane was handed to a successor holder: this process never reads
    /// the master again (two readers of one master split its stream).
    HandedOff,
}

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
    /// Whether the reader may read (plan Phase 8).
    pub read_gate: ReadGate,
    /// The child's exit as the waiter observed it, recorded BEFORE the exit
    /// is settled — so a holder upgrade that commits while the settle waits
    /// on a parked reader can still hand the real code to its successor.
    pub observed_exit: Option<ExitReply>,
    /// The reader is parked at the gate: no read is in flight, and none will
    /// start until the gate is [`ReadGate::Run`] again.
    pub read_parked: bool,
}

/// The PTY master a pane drives.
enum Master {
    /// Opened by this holder through `portable-pty`.
    Portable(Box<dyn MasterPty + Send>),
    /// Inherited from a predecessor holder (plan Phase 8): `portable-pty` has
    /// no constructor for an existing fd (D12a rule 4), so it is a plain file
    /// resized with `TIOCSWINSZ`.
    #[cfg(unix)]
    Adopted(std::fs::File),
}

impl Master {
    fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        match self {
            Master::Portable(m) => m
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| other(format!("resize: {e}"))),
            #[cfg(unix)]
            Master::Adopted(f) => {
                use std::os::unix::io::AsRawFd;
                set_winsize(f.as_raw_fd(), cols, rows)
            }
        }
    }

    #[cfg(unix)]
    fn raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        use std::os::unix::io::AsRawFd;
        match self {
            Master::Portable(m) => m.as_raw_fd(),
            Master::Adopted(f) => Some(f.as_raw_fd()),
        }
    }
}

/// `TIOCSWINSZ` on a PTY master.
#[cfg(unix)]
pub fn set_winsize(fd: std::os::unix::io::RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads a winsize from a valid pointer; `fd` is open.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws as *const libc::winsize) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `TIOCGWINSZ` on a PTY master: (cols, rows).
#[cfg(unix)]
pub fn get_winsize(fd: std::os::unix::io::RawFd) -> io::Result<(u16, u16)> {
    // SAFETY: winsize is plain data; zeroed is a valid value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes a winsize through a valid pointer; `fd` is open.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut ws as *mut libc::winsize) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((ws.ws_col, ws.ws_row))
}

/// A new `O_CLOEXEC` descriptor for the same open file as `fd`, numbered at
/// least `min`.
#[cfg(unix)]
pub fn dup_cloexec(
    fd: std::os::unix::io::RawFd,
    min: i32,
) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::FromRawFd;
    // SAFETY: F_DUPFD_CLOEXEC on an fd the caller keeps open for the call.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, min) };
    if new < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `new` was just returned by fcntl and is owned by nobody else.
    Ok(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(new) })
}

/// The reader's wake-up pipe (Unix): a byte written to it makes a reader
/// blocked in `poll` re-check its gate, without touching the master.
#[cfg(unix)]
struct WakePipe {
    read: std::os::unix::io::OwnedFd,
    write: std::os::unix::io::OwnedFd,
}

#[cfg(unix)]
impl WakePipe {
    fn new() -> io::Result<WakePipe> {
        use std::os::unix::io::FromRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe() writes two fds into a valid two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both fds were just returned by pipe() and are owned here.
        let (read, write) = unsafe {
            (
                std::os::unix::io::OwnedFd::from_raw_fd(fds[0]),
                std::os::unix::io::OwnedFd::from_raw_fd(fds[1]),
            )
        };
        for fd in fds {
            // SAFETY: fcntl on fds owned above.
            unsafe {
                let fd_flags = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        Ok(WakePipe { read, write })
    }

    fn wake(&self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: a one-byte write from a valid buffer to an owned fd. A full
        // pipe (EAGAIN) already holds a wake-up, which is all that matters.
        unsafe {
            libc::write(self.write.as_raw_fd(), [1u8].as_ptr().cast(), 1);
        }
    }

    fn drain(&self) {
        use std::os::unix::io::AsRawFd;
        let mut buf = [0u8; 64];
        // SAFETY: reads into a valid buffer from an owned, non-blocking fd.
        while unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {
        }
    }
}

/// Where a pane's input stands (see [`Pane::write_input`], [`Pane::freeze_input`]).
#[derive(Debug, Default)]
struct InputState {
    /// The generation of the CURRENT input connection.
    generation: u64,
    /// A holder upgrade froze input: new frames are HELD, not queued.
    frozen: bool,
    held: Vec<Vec<u8>>,
    held_bytes: usize,
    /// A committing upgrade is DRAINING the input connections: the hold cap
    /// is lifted (what the sockets already carry is bounded by their
    /// buffers), so a frame is held, never dropped (review r1, finding 3).
    draining: bool,
    /// No new input connection is accepted (a committing upgrade).
    refuse_new: bool,
    /// The pane was handed to a successor: nothing more is accepted here.
    handed_off: bool,
    /// Unix: every open input connection, so a committing upgrade can stop
    /// their reads and drain what they carry (`Pane::drain_input_for_handoff`).
    #[cfg(unix)]
    conns: Vec<(u64, crate::transport::Conn)>,
    #[cfg(unix)]
    next_conn: u64,
}

/// The pane a holder owns.
pub struct Pane {
    child_pid: u32,
    /// The directory the child was started in (the spec's), for restore
    /// metadata when the program never reports one with `OSC 7`.
    spawn_cwd: Option<std::ffi::OsString>,
    state: Mutex<PaneState>,
    changed: Condvar,
    master: Mutex<Master>,
    /// Input for the writer thread (see the module docs).
    input_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// The generation of the CURRENT input connection, and the upgrade freeze.
    /// `open_input` on a new connection bumps the generation
    /// ([`Pane::supersede_input`]); every enqueue checks its own generation
    /// against it UNDER this lock, so once a newer input connection exists an
    /// older one can enqueue nothing more — its bytes can never land after (or
    /// interleave with) the newer connection's.
    input: Mutex<InputState>,
    /// Input frames queued to the writer and not yet written (or discarded).
    /// Zero means nothing of the input is in this process but held frames.
    pending_input: AtomicUsize,
    /// Unix: wakes the reader to re-check its gate (plan Phase 8).
    #[cfg(unix)]
    wake: WakePipe,
    /// The child is not this process's own: it came with an adopted master
    /// (plan Phase 8). No `waitpid` — the exit is master EOF, its code
    /// unknown — and every signal is preceded by a start-token check.
    #[cfg(unix)]
    adopted: bool,
    /// An adopted pane's child exit as its predecessor observed it (it exited
    /// mid-swap and the predecessor, its parent, reaped it).
    #[cfg(unix)]
    known_exit: Mutex<Option<ExitReply>>,
    #[cfg(windows)]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// Windows: a holder-owned `KILL_ON_JOB_CLOSE` job holding the child, so
    /// `kill` ends the child's whole TREE (`TerminateJobObject`), not the child
    /// alone. `None` when the child could not be assigned (then `kill` falls
    /// back to terminating the child only, and says so in the holder log).
    #[cfg(windows)]
    job: Option<win_job::Job>,
    exit_linger: Duration,
    /// How many bytes the output ring keeps (a holder upgrade's snapshot).
    #[cfg(unix)]
    ring_capacity: usize,
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

/// What a successor holder adopts (plan Phase 8): everything
/// [`Pane::adopt`] needs, read from the predecessor's snapshot plus the
/// inherited master.
#[cfg(unix)]
pub struct AdoptedParts {
    pub master: std::fs::File,
    pub child_pid: u32,
    pub child_start: Option<String>,
    pub spawn_cwd: Option<std::ffi::OsString>,
    /// The ring as the predecessor left it, ending at the handoff offset.
    pub ring: OutputRing,
    pub ring_capacity: usize,
    pub modes: ModeTracker,
    /// The size the predecessor recorded; the master's own is preferred.
    pub size: (u16, u16),
    pub exit_linger: Duration,
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
        // Unix: the reader is this process's own dup of the master, polled
        // with the wake pipe so a holder upgrade can stop it between reads.
        #[cfg(unix)]
        let reader = {
            let fd = master
                .as_raw_fd()
                .ok_or_else(|| other("master has no fd"))?;
            std::fs::File::from(dup_cloexec(fd, 0)?)
        };
        #[cfg(windows)]
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
                read_gate: ReadGate::Run,
                read_parked: false,
                observed_exit: None,
            }),
            changed: Condvar::new(),
            master: Mutex::new(Master::Portable(master)),
            input_tx,
            input: Mutex::new(InputState::default()),
            pending_input: AtomicUsize::new(0),
            #[cfg(unix)]
            wake: WakePipe::new()?,
            #[cfg(unix)]
            adopted: false,
            #[cfg(unix)]
            known_exit: Mutex::new(None),
            #[cfg(windows)]
            killer: Mutex::new(killer),
            #[cfg(windows)]
            job,
            exit_linger: spec.exit_linger(),
            #[cfg(unix)]
            ring_capacity: spec.ring_capacity(),
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

    /// The child is not this process's own (plan Phase 8).
    #[cfg(unix)]
    pub fn is_adopted(&self) -> bool {
        self.adopted
    }

    /// How many bytes the output ring keeps.
    #[cfg(unix)]
    pub fn ring_capacity(&self) -> usize {
        self.ring_capacity
    }

    /// How long this holder lingers for an uncollected `exit` frame.
    #[cfg(unix)]
    pub fn exit_linger(&self) -> Duration {
        self.exit_linger
    }

    /// Build a pane from a master inherited from a predecessor holder (plan
    /// Phase 8, D12a). The reader starts PARKED ([`ReadGate::Quiesce`]): the
    /// predecessor may still roll back and resume reading, and two readers of
    /// one master split its stream (D12a rule 1). [`Pane::resume_reads`] once
    /// the predecessor has committed the handoff. There is no waiter thread:
    /// the child is not ours, and its exit is the master's EOF.
    #[cfg(unix)]
    pub fn adopt(parts: AdoptedParts) -> io::Result<Arc<Pane>> {
        use std::os::unix::io::AsRawFd;
        let fd = parts.master.as_raw_fd();
        let reader = std::fs::File::from(dup_cloexec(fd, 0)?);
        let writer = std::fs::File::from(dup_cloexec(fd, 0)?);
        let size = get_winsize(fd).unwrap_or(parts.size);
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE_FRAMES);
        let pane = Arc::new(Pane {
            child_pid: parts.child_pid,
            spawn_cwd: parts.spawn_cwd,
            state: Mutex::new(PaneState {
                ring: parts.ring,
                modes: parts.modes,
                eof: false,
                child_gone: false,
                exit: None,
                exit_delivered: false,
                ended_by_kill: None,
                size,
                read_gate: ReadGate::Quiesce,
                read_parked: false,
                observed_exit: None,
            }),
            changed: Condvar::new(),
            master: Mutex::new(Master::Adopted(parts.master)),
            input_tx,
            input: Mutex::new(InputState::default()),
            pending_input: AtomicUsize::new(0),
            wake: WakePipe::new()?,
            adopted: true,
            known_exit: Mutex::new(None),
            exit_linger: parts.exit_linger,
            ring_capacity: parts.ring_capacity,
            child_start: parts.child_start,
            session_reap: Mutex::new(None),
            kill_reached: AtomicBool::new(false),
        });
        let p = Arc::clone(&pane);
        std::thread::Builder::new()
            .name("pty-holder-reader".into())
            .spawn(move || p.read_loop(reader))?;
        let p = Arc::clone(&pane);
        let writer: Box<dyn Write + Send> = Box::new(writer);
        std::thread::Builder::new()
            .name("pty-holder-writer".into())
            .spawn(move || p.write_loop(writer, input_rx))?;
        Ok(pane)
    }

    /// The master's fd, for a holder upgrade to pass to its successor.
    #[cfg(unix)]
    pub fn master_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        self.master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .raw_fd()
    }

    /// Plan Phase 8, D12a rule 1: stop the reader BETWEEN reads and return the
    /// stream offset it stopped at — every byte before it is in the ring,
    /// none after it has left the kernel. Waits at most `bound` for the reader
    /// to park; refuses a pane whose output has ended or whose child is gone
    /// (there is nothing left to hand over). On any failure the gate is back
    /// at [`ReadGate::Run`].
    #[cfg(unix)]
    pub fn quiesce_reads(&self, bound: Duration) -> io::Result<u64> {
        let refused = |why: &str| Err(io::Error::other(why.to_string()));
        {
            let mut st = self.lock_state();
            if st.read_gate != ReadGate::Run {
                return refused("the reader is already quiesced or handed off");
            }
            if st.eof || st.child_gone || st.exit.is_some() {
                return refused("the pane's child has exited");
            }
            st.read_gate = ReadGate::Quiesce;
        }
        self.changed.notify_all();
        self.wake.wake();
        let deadline = Instant::now() + bound;
        let mut st = self.lock_state();
        loop {
            if st.read_parked {
                return Ok(st.ring.end_offset());
            }
            let now = Instant::now();
            if st.eof || now >= deadline {
                let why = if st.eof {
                    "the pane's output ended while quiescing"
                } else {
                    "the reader did not park in time"
                };
                st.read_gate = ReadGate::Run;
                drop(st);
                self.changed.notify_all();
                self.wake.wake();
                return refused(why);
            }
            st = self.wait_changed(st, deadline - now);
        }
    }

    /// Undo [`Pane::quiesce_reads`]: the reader resumes exactly where it
    /// stopped. A rolled-back upgrade, or an adopted pane once its
    /// predecessor has committed.
    #[cfg(unix)]
    pub fn resume_reads(&self) {
        self.lock_state().read_gate = ReadGate::Run;
        self.changed.notify_all();
        self.wake.wake();
    }

    /// The pane now belongs to a successor holder: this process never reads
    /// its master again.
    #[cfg(unix)]
    pub fn hand_off_reads(&self) {
        self.lock_state().read_gate = ReadGate::HandedOff;
        self.changed.notify_all();
        self.wake.wake();
    }

    /// Plan Phase 8: from now on input is HELD rather than queued to the
    /// writer, and every frame already queued must reach the child within
    /// `bound` — the writer may be mid-write when the upgrade commits, and a
    /// partly written frame cannot be handed over. A child that does not drain
    /// its input in time fails the freeze (the input is unfrozen again).
    #[cfg(unix)]
    pub fn freeze_input(&self, bound: Duration) -> io::Result<()> {
        self.input.lock().unwrap_or_else(|p| p.into_inner()).frozen = true;
        let deadline = Instant::now() + bound;
        while self.pending_input.load(Ordering::SeqCst) != 0 {
            if Instant::now() >= deadline {
                self.unfreeze_input();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the child did not read its queued input in time",
                ));
            }
            std::thread::sleep(INPUT_QUEUE_POLL);
        }
        Ok(())
    }

    /// Undo [`Pane::freeze_input`]: what was held goes to the writer FIRST,
    /// in order, before any later frame (the enqueue happens under the input
    /// lock that later frames must take).
    #[cfg(unix)]
    pub fn unfreeze_input(&self) {
        let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        let held = std::mem::take(&mut input.held);
        input.held_bytes = 0;
        for frame in held {
            self.pending_input.fetch_add(1, Ordering::SeqCst);
            // The queue was drained by the freeze and held frames are capped
            // at its capacity, so this does not wait in practice; a blocking
            // send keeps the order if it ever must.
            if self.input_tx.send(frame).is_err() {
                self.pending_input.fetch_sub(1, Ordering::SeqCst);
            }
        }
        input.frozen = false;
        input.draining = false;
        input.refuse_new = false;
    }

    /// Register an input connection (`open_input`), so a committing upgrade
    /// can drain it. `None` while an upgrade is committing: the connection is
    /// then refused BEFORE its `ok`, so its client sends nothing here and
    /// reconnects — to the successor.
    #[cfg(unix)]
    pub fn register_input_conn(&self, conn: crate::transport::Conn) -> Option<u64> {
        let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        if input.refuse_new || input.handed_off {
            return None;
        }
        input.next_conn += 1;
        let id = input.next_conn;
        input.conns.push((id, conn));
        Some(id)
    }

    /// The input connection `id` has ended (its dispatch loop left).
    #[cfg(unix)]
    pub fn unregister_input_conn(&self, id: u64) {
        let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        input.conns.retain(|(i, _)| *i != id);
    }

    /// The commit's input drain (review r1, finding 3): refuse new input
    /// connections, lift the hold cap, `shutdown(SHUT_RD)` every open one and
    /// wait — at most `bound` — until each dispatch has read what its socket
    /// already held (into the held input) and ended. From then on a client
    /// write to one of them fails (`EPIPE`) instead of vanishing; a frame cut
    /// in half by the shutdown is discarded whole, and its writer saw the
    /// failure. A drain that does not finish in time fails (`unfreeze_input`
    /// re-opens input on a rollback).
    #[cfg(unix)]
    pub fn drain_input_for_handoff(&self, bound: Duration) -> io::Result<()> {
        {
            let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
            input.refuse_new = true;
            input.draining = true;
            for (_, conn) in &input.conns {
                conn.shutdown_read();
            }
        }
        let deadline = Instant::now() + bound;
        loop {
            if self
                .input
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .conns
                .is_empty()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the input connections did not drain in time",
                ));
            }
            std::thread::sleep(INPUT_QUEUE_POLL);
        }
    }

    /// Commit point of a holder upgrade: take the held input (it travels to
    /// the successor) and refuse every later frame. Undone by
    /// [`Pane::restore_handed_input`] if the successor cannot be told.
    #[cfg(unix)]
    pub fn take_input_for_handoff(&self) -> Vec<Vec<u8>> {
        let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        input.handed_off = true;
        input.held_bytes = 0;
        std::mem::take(&mut input.held)
    }

    /// The commit could not be delivered: accept input again, `held` first.
    #[cfg(unix)]
    pub fn restore_handed_input(&self, mut held: Vec<Vec<u8>>) {
        {
            let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
            input.handed_off = false;
            held.append(&mut input.held);
            input.held_bytes = held.iter().map(Vec::len).sum();
            input.held = held;
        }
        self.unfreeze_input();
    }

    /// An adopted pane: the child's exit code its predecessor observed.
    #[cfg(unix)]
    pub fn set_known_exit(&self, exit: ExitReply) {
        *self.known_exit.lock().unwrap_or_else(|p| p.into_inner()) = Some(exit);
    }

    /// An adopted pane: queue the input its predecessor held during the
    /// handoff, ahead of anything a client sends here.
    #[cfg(unix)]
    pub fn inject_input(&self, frames: Vec<Vec<u8>>) {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        for frame in frames {
            self.pending_input.fetch_add(1, Ordering::SeqCst);
            if self.input_tx.send(frame).is_err() {
                self.pending_input.fetch_sub(1, Ordering::SeqCst);
            }
        }
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

    /// One chunk the reader read: into the ring and the mode tracker.
    fn absorb_output(&self, bytes: &[u8]) {
        let mut st = self.lock_state();
        st.ring.push(bytes);
        st.modes.feed(bytes);
        let ring_start = st.ring.start_offset();
        st.modes.forget_before(ring_start);
        drop(st);
        self.changed.notify_all();
    }

    #[cfg(windows)]
    fn read_loop(&self, mut reader: Box<dyn Read + Send>) {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.absorb_output(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        self.lock_state().eof = true;
        self.changed.notify_all();
    }

    /// Unix: read the master whenever it is readable AND the gate is
    /// [`ReadGate::Run`]. Between reads the reader checks the gate; a holder
    /// upgrade parks it there (`read_parked`), so the stream position is exact
    /// and no byte is half-consumed. `poll` covers the master and the wake
    /// pipe, so a parked request reaches a reader that is waiting for output.
    #[cfg(unix)]
    fn read_loop(&self, mut reader: std::fs::File) {
        use std::os::unix::io::AsRawFd;
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            {
                let mut st = self.lock_state();
                loop {
                    match st.read_gate {
                        ReadGate::Run => break,
                        // Never touch the master again, and do not report
                        // EOF: the successor owns the stream now.
                        ReadGate::HandedOff => return,
                        ReadGate::Quiesce => {
                            if !st.read_parked {
                                st.read_parked = true;
                                self.changed.notify_all();
                            }
                            st = self.wait_changed(st, Duration::from_secs(60));
                        }
                    }
                }
                if st.read_parked {
                    st.read_parked = false;
                    self.changed.notify_all();
                }
            }
            let mut fds = [
                libc::pollfd {
                    fd: reader.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: std::os::unix::io::AsRawFd::as_raw_fd(&self.wake.read),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: poll over a valid two-element array of open fds.
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if rc < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if fds[1].revents != 0 {
                // A gate change (or a stale wake-up): re-check before reading.
                self.wake.drain();
                continue;
            }
            if fds[0].revents == 0 {
                continue;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.absorb_output(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Linux reports a hung-up PTY master as EIO, not 0: both end it.
                Err(_) => break,
            }
        }
        self.lock_state().eof = true;
        self.changed.notify_all();
        if self.adopted {
            // D12a rule 3: this holder is not the child's parent, so master
            // EOF/EIO IS the exit, and its code is UNKNOWN — never 0, never
            // a fabricated failure — unless the predecessor observed it
            // during the swap and sent it with the commit.
            let known = *self.known_exit.lock().unwrap_or_else(|p| p.into_inner());
            self.settle(known.unwrap_or(ExitReply {
                code: None,
                signal: None,
            }));
        }
    }

    /// Settle the exit once the child is gone and its output has drained.
    fn settle(&self, exit: ExitReply) {
        let mut st = self.lock_state();
        st.child_gone = true;
        let killed = self.kill_reached.load(Ordering::SeqCst);
        st.ended_by_kill.get_or_insert(killed);
        st.observed_exit = Some(exit);
        // Plan Phase 8: while a holder upgrade has the reader parked, the
        // child's last output may still be in the kernel, unread. The exit is
        // not settled until the reader runs again (a rolled-back upgrade) —
        // and never here once the pane was handed off: the successor reads
        // that output and reports the exit.
        while st.read_gate != ReadGate::Run {
            st = self.wait_changed(st, Duration::from_secs(60));
        }
        let deadline = Instant::now() + EXIT_DRAIN_GRACE;
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
                let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
                if input.generation != generation {
                    return Err(io::Error::other(
                        "superseded: a newer input connection replaced this one",
                    ));
                }
                if input.handed_off {
                    return Err(io::Error::other(
                        "handed off: this pane is now served by a successor holder",
                    ));
                }
                if input.frozen {
                    // A holder upgrade is in progress: HOLD the frame (it
                    // goes to the successor, or back to this writer on a
                    // rollback). Past the bound, wait like a full queue.
                    if input.draining
                        || (input.held.len() < INPUT_QUEUE_FRAMES
                            && input.held_bytes + chunk.len() <= MAX_HELD_INPUT_BYTES)
                    {
                        input.held_bytes += chunk.len();
                        input.held.push(chunk);
                        return Ok(());
                    }
                    Err(TrySendError::Full(chunk))
                } else {
                    self.pending_input.fetch_add(1, Ordering::SeqCst);
                    let sent = self.input_tx.try_send(chunk);
                    if sent.is_err() {
                        self.pending_input.fetch_sub(1, Ordering::SeqCst);
                    }
                    sent
                }
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
            if !broken
                && !self.lock_state().child_gone
                && w.write_all(&bytes).and_then(|()| w.flush()).is_err()
            {
                broken = true;
            }
            self.pending_input.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Make the caller the pane's CURRENT input connection and return its
    /// generation. Every older input connection is superseded from this
    /// instant: its pending and future input is discarded (see
    /// [`Pane::write_input`]). Bytes it had already enqueued stay queued —
    /// they precede everything the new connection sends.
    pub fn supersede_input(&self) -> u64 {
        let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        input.generation = input.generation.wrapping_add(1);
        input.generation
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
            .resize(cols, rows)?;
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
        if self.adopted {
            // Not our child (plan Phase 8): no waitid, and its pid could have
            // been reaped and reused by now. Signal only the process the
            // start token proves is the pane's child — no token, no signal.
            // (A check-then-kill: the window is the few instructions between
            // the /proc read and kill(2).)
            if !crate::proc_identity::is_same_process(self.child_pid, self.child_start.as_deref()) {
                return false;
            }
        } else {
            // The child may have exited on its own an instant ago, before
            // the waiter marked it gone: then this kill reaches nothing, and
            // must not turn its normal exit into a kill (review round 2,
            // N2). A zombie is seen without reaping it (`WNOWAIT`), so the
            // waiter's reap is intact.
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
            if self.adopted {
                report.note(
                    "adopted pane (holder upgrade): its child's session was not swept — this \
                     holder is not the child's parent, so the session id is not pinned",
                );
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
