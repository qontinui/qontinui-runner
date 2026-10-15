//! The holder side: lock, child, endpoint, and the per-connection dispatch.
//!
//! Start-up order is plan D3, per pane (D13), with Phase 2's child in it:
//!
//! 1. Make sure the pane directory exists and (Unix) is ours and 0700.
//! 2. Take `<pane-id>.lock`, non-blocking. Held by a live holder → exit,
//!    touching nothing (not even the spec). Held while its record names no
//!    live holder → a transient hold, waited out briefly first
//!    (`crate::lock::PaneLock::try_acquire_unless_live`, finding 9bbe1079).
//! 3. Consume `<pane-id>.spec` (`crate::spec`): read it, unlink it.
//! 4. Open the PTY and spawn the child on it (`crate::pty`).
//! 5. Write the [`LockRecord`] (holder pid, CHILD pid, versions, start time).
//! 6. Unix: a `<pane-id>.sock` left by a dead holder is unlinked — only now,
//!    under the lock, when no live holder can own it.
//! 7. Bind the endpoint. A failure from 5 on ends the child it just spawned:
//!    a pane nobody can reach is a leak.
//!
//! The dispatch is a POSITIVE ALLOWLIST (plan D5): every frame kind, verb and
//! state combination not named in [`handle_request`] / [`handle_data`] falls
//! through to a typed `rejected` and the connection is closed. There is no
//! default action.
//!
//! ## The attached connection (protocol version 2)
//!
//! After `attach`, a connection has two threads. The dispatch loop keeps
//! reading requests and input frames; a PUMP thread writes the pane's output to
//! it from the ring (`crate::pty`), at the connection's own offset. Every write
//! — a reply, an output frame — goes through one lock around the connection's
//! write half ([`Conn::try_clone`]), and each is a single framed write, so the
//! two threads never interleave inside a frame. The pump sends:
//!
//! - `output_lost {from, to}` whenever its next offset has rolled out of the
//!   ring (a resume from too far back, or a client too slow to keep up);
//! - `KIND_OUTPUT` frames, at most [`MAX_OUTPUT_CHUNK`] bytes each;
//! - once the child's exit is settled and everything before it is sent, one
//!   `exit` frame — then it marks the exit delivered, which lets the holder
//!   exit (`crate::pty::Pane::wait_for_delivery`).
//!
//! ## The restore envelope (protocol version 3, plan D14)
//!
//! An `attach {restore: true}` on a version-3 connection is answered with
//! `restore {cwd, title}` metadata in `attached`, and the tail is bracketed by
//! two synthetic frames — ALWAYS both, empty or not, so a client can tell a
//! delivered postamble from one lost with the connection. The pump computes
//! them from one look at `crate::modes::ModeTracker` under the pane lock:
//!
//! - the PREAMBLE (state at the first byte the connection can still be sent)
//!   is the pump's first frame — after a loss report only when the requested
//!   offset has already left the ring — and precedes all output;
//! - the POSTAMBLE (state at the stream's end at that moment) is held and sent
//!   the moment the stream reaches that end — output frames are cut at the
//!   anchor, and a loss report that would jump past it is preceded by it — so
//!   it lands before any byte produced later and before the `exit`.
//!
//! `pause` stops the pump's SENDING only; the PTY read never pauses. A pump
//! whose client stops reading for [`PUMP_WRITE_TIMEOUT`] drops the connection
//! (the client reattaches at its last offset), so a wedged client cannot pin
//! the pane.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::frame::{
    output_payload, read_frame, synthetic_payload, write_frame, SyntheticPart, KIND_CONTROL,
    KIND_DATA, KIND_OUTPUT, KIND_SYNTHETIC,
};
use crate::lock::{LockRecord, PaneLock, TryLock};
use crate::pane::{lock_path, log_path, prepare_private_dir, PaneId};
use crate::protocol::{
    holder_build, negotiate, parse_request, to_payload, AttachedReply, CensusReply, ExitReply,
    HelloAck, RejectReason, Reply, Request, ATTACH_TAIL_BYTES, DATA_PATH_VERSION,
    PROTOCOL_VERSIONS, RESTORE_VERSION,
};
use crate::pty::Pane;
use crate::transport::{self, Conn, DeadlineIo, Listener};

/// Why a non-Unix holder answers `prepare_upgrade` with `unsupported` (plan
/// D12 / Phase 8): a ConPTY handle cannot be handed to another process. The
/// honest actions for an incompatible or defective holder there are "leave it
/// running" and "end it and resume" (Phase 6) — never "upgrade".
pub const UPGRADE_UNSUPPORTED_ON_THIS_OS: &str = "holder upgrade is Unix-only (plan D12a): a \
     ConPTY cannot be handed to another process; leave this holder running, or end the pane \
     and resume it";

/// Whether a holder on THIS platform can be upgraded in place (plan Phase 8).
/// What a UI offering "upgrade the holder" must check: only `true` on Unix.
pub const fn upgrade_supported() -> bool {
    cfg!(unix)
}

/// Most concurrent connections one holder serves. Each costs a thread (two
/// once attached); the runner needs one or two. Past this a client gets
/// `rejected {busy}`.
pub const MAX_CONNECTIONS: usize = 16;

/// How long a fresh connection has to send its `hello`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on writing one reply.
pub const REPLY_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on writing one output frame. A client that has not drained its socket
/// for this long is dropped; it can reattach at its last offset.
pub const PUMP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest an input frame waits for room in the pane's input queue (the child
/// has not read the last few MiB) before its connection is closed. Longer than
/// the runner's own input deadline (`DaemonPaneIo`'s `INPUT_DEADLINE`, 60 s),
/// so a live runner gives up first and reattaches; this bound frees the
/// connection's thread and slot when nobody does.
pub const INPUT_QUEUE_TIMEOUT: Duration = Duration::from_secs(90);

/// Largest output frame payload (offset header excluded).
pub const MAX_OUTPUT_CHUNK: usize = 64 * 1024;

/// How often an idle pump re-checks its flags even without a wake-up.
const PUMP_POLL: Duration = Duration::from_millis(250);

/// First pause after a failed accept.
pub const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(10);
/// Longest pause between accept retries.
pub const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
/// The pause after an accept error the transport calls fatal. The holder
/// does not exit on one (it holds a live pane); it keeps retrying this slowly.
pub const ACCEPT_BACKOFF_FATAL: Duration = Duration::from_secs(5);

/// Largest the holder's diagnostic log may grow; later lines are dropped.
pub const MAX_LOG_BYTES: u64 = 256 * 1024;

/// Why a holder could not start.
#[derive(Debug)]
pub enum StartError {
    /// Another live process holds this pane's lock.
    LockHeld {
        /// Boxed: a record is large next to every other variant.
        record: Option<Box<LockRecord>>,
    },
    /// The pane directory is unusable (not absolute, not ours, cannot be made
    /// private).
    PaneDir(String),
    /// The child spec is missing, refused or unreadable.
    Spec(io::Error),
    /// The PTY could not be opened or the child could not be spawned.
    Child(io::Error),
    Io(io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::LockHeld { record: Some(r) } => {
                write!(f, "lock held by holder pid {}", r.holder_pid)
            }
            StartError::LockHeld { record: None } => {
                write!(f, "lock held by an unidentified holder")
            }
            StartError::PaneDir(m) => write!(f, "pane dir: {m}"),
            StartError::Spec(e) => write!(f, "spec: {e}"),
            StartError::Child(e) => write!(f, "child: {e}"),
            StartError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<io::Error> for StartError {
    fn from(e: io::Error) -> Self {
        StartError::Io(e)
    }
}

/// Identity of a running holder, shared with every connection thread.
#[derive(Debug, Clone)]
pub struct HolderInfo {
    pub pane_id: PaneId,
    pub holder_pid: u32,
    pub child_pid: Option<u32>,
    pub started_at_unix_ms: u64,
    /// What `prepare_upgrade` needs (plan Phase 8, Unix). `None` in a holder
    /// with no pane (dispatch unit tests), which answers `unsupported`.
    #[cfg(unix)]
    pub upgrade: Option<crate::handoff::UpgradeCtx>,
}

/// A holder that holds its lock, owns its pane's child, and has bound its
/// endpoint.
#[derive(Debug)]
pub struct Holder {
    info: Arc<HolderInfo>,
    pane: Arc<Pane>,
    listener: Listener,
    log: PathBuf,
    /// The pane's durable tail (plan D7): written by its own timer thread and
    /// once more when the holder ends.
    scrollback: Arc<Mutex<crate::scrollback::ScrollbackWriter>>,
    // Held for the holder's lifetime; dropping it releases the pane.
    lock: PaneLock,
    /// What `lock` holds, kept to rewrite it when the pane ends normally.
    record: LockRecord,
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append one line to the holder's size-capped diagnostic log. Best effort: a
/// log that cannot be written is not a reason to touch the pane.
pub fn log_line(log: &Path, msg: &str) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() >= MAX_LOG_BYTES) {
        return;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(log) {
        let _ = writeln!(f, "{} pid={} {msg}", now_unix_ms(), std::process::id());
    }
}

impl Holder {
    /// Steps 1–7 of the module docs. Nothing is bound unless the lock is ours
    /// and the child is running.
    pub fn start(pane_dir: &Path, pane_id: &PaneId) -> Result<Holder, StartError> {
        prepare_private_dir(pane_dir).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied => {
                StartError::PaneDir(e.to_string())
            }
            _ => StartError::Io(e),
        })?;
        let lock_file = lock_path(pane_dir, pane_id);
        // A hold by no live holder (a fork-before-exec, a prober) is waited
        // out briefly; a live holder is never retried against (`crate::lock`).
        let mut lock = match PaneLock::try_acquire_unless_live(&lock_file)? {
            TryLock::Acquired(l) => l,
            TryLock::Held => {
                return Err(StartError::LockHeld {
                    record: crate::lock::read_record(&lock_file).map(Box::new),
                })
            }
        };
        let spec = crate::spec::consume_spec(pane_dir, pane_id).map_err(StartError::Spec)?;
        let pane = Pane::spawn(&spec).map_err(StartError::Child)?;
        let info = HolderInfo {
            pane_id: pane_id.clone(),
            holder_pid: std::process::id(),
            child_pid: Some(pane.child_pid()),
            started_at_unix_ms: now_unix_ms(),
            #[cfg(unix)]
            upgrade: Some(crate::handoff::UpgradeCtx {
                pane_dir: pane_dir.to_path_buf(),
                lock_fd: lock.as_raw_fd(),
                log: log_path(pane_dir, pane_id),
            }),
        };
        let record = LockRecord {
            holder_pid: info.holder_pid,
            child_pid: info.child_pid,
            versions: PROTOCOL_VERSIONS.to_vec(),
            started_at_unix_ms: info.started_at_unix_ms,
            holder_build: holder_build(),
            child_start: pane.child_start().map(str::to_string),
            holder_start: crate::proc_identity::start_token(info.holder_pid),
            scope_unit: own_scope_unit(),
            ended: None,
        };
        let bound = (|| -> io::Result<Listener> {
            lock.write_record(&record)?;
            #[cfg(unix)]
            {
                let sock = crate::pane::socket_path(pane_dir, pane_id);
                match std::fs::remove_file(&sock) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            transport::bind(pane_dir, pane_id)
        })();
        let listener = match bound {
            Ok(l) => l,
            Err(e) => {
                // Unreachable pane: end the child rather than leak it.
                pane.kill_now();
                return Err(StartError::Io(e));
            }
        };
        let scrollback =
            crate::scrollback::ScrollbackWriter::new(pane_dir, pane_id, pane.child_pid());
        Ok(Holder {
            info: Arc::new(info),
            pane,
            listener,
            log: log_path(pane_dir, pane_id),
            scrollback: Arc::new(Mutex::new(scrollback)),
            lock,
            record,
        })
    }

    pub fn info(&self) -> &HolderInfo {
        &self.info
    }

    /// A holder assembled by a holder upgrade's successor (plan Phase 8,
    /// `crate::handoff::adopt`): the lock it inherited, the pane it adopted
    /// and the endpoint it took over. It then [`Holder::run`]s like any other.
    #[cfg(unix)]
    pub(crate) fn from_parts(
        info: HolderInfo,
        pane: Arc<Pane>,
        listener: Listener,
        log: PathBuf,
        lock: PaneLock,
        record: LockRecord,
    ) -> Holder {
        // Its own durable-tail writer for the same child (plan D7); the log
        // sits in the pane dir.
        let pane_dir = log.parent().map(Path::to_path_buf).unwrap_or_default();
        let scrollback =
            crate::scrollback::ScrollbackWriter::new(&pane_dir, &info.pane_id, pane.child_pid());
        Holder {
            info: Arc::new(info),
            pane,
            listener,
            log,
            scrollback: Arc::new(Mutex::new(scrollback)),
            lock,
            record,
        }
    }

    pub fn pane(&self) -> &Arc<Pane> {
        &self.pane
    }

    /// Where the endpoint lives, for the ready line.
    pub fn endpoint(&self, pane_dir: &Path) -> PathBuf {
        #[cfg(unix)]
        {
            let _ = pane_dir;
            self.listener.path().to_path_buf()
        }
        #[cfg(windows)]
        {
            PathBuf::from(crate::pane::pipe_name(pane_dir, &self.info.pane_id))
        }
    }

    /// Serve until the holder may exit (`crate::pty::Pane::wait_for_delivery`) and
    /// return the child's exit. The accept loop runs on its own thread and
    /// never ends the holder; the lock is held until this returns. The durable
    /// tail is persisted on its timer throughout, and once more — the child's
    /// last output — before the lock is released.
    pub fn run(self) -> ExitReply {
        let Holder {
            info,
            pane,
            listener,
            log,
            scrollback,
            mut lock,
            mut record,
        } = self;
        if let Err(e) = crate::scrollback::spawn_persister(
            Arc::clone(&pane),
            Arc::clone(&scrollback),
            log.clone(),
        ) {
            // No timer: the pane runs on; its tail is still persisted at the
            // end below.
            log_line(&log, &format!("scrollback thread: {e}"));
        }
        if let Err(e) = spawn_accept_loop(listener, info, Arc::clone(&pane), log.clone()) {
            // No accept loop: the pane runs on, unreachable but alive and
            // counted (its lock is held), until the child exits.
            eprintln!("pty-holder: accept thread: {e}");
        }
        let exit = pane.wait_for_exit();
        // A NORMAL end (the child exited on its own) must never cost the
        // child's leftovers — not now, and not at a later boot sweep (plan D8
        // as decided 2026-10-14). So, still under the lock: the record says
        // the pane ENDED BY EXIT (a sweep then cleans its files and signals
        // nothing), and on Windows the job stops killing on close.
        let mut end_errors: Vec<String> = Vec::new();
        if pane.ended_by_kill() != Some(true) {
            record.ended = Some(crate::lock::ENDED_BY_EXIT.to_string());
            if let Err(e) = lock.write_record(&record) {
                // Review round 3, R4: a record that still reads `ended: None`
                // would let the next sweep reap the leftovers. An EMPTY one
                // makes it signal nothing (no record, nothing provable).
                let cleared = lock.clear_record();
                end_errors.push(format!(
                    "ERROR: could not mark the record ended-by-exit ({e}); cleared it instead: \
                     {cleared:?}"
                ));
            }
            if let Err(e) = pane.release_leftovers_on_normal_exit() {
                end_errors.push(format!(
                    "ERROR: could not disarm the job's kill-on-close ({e}): the child's \
                     leftovers will end when this holder exits"
                ));
            }
            for e in &end_errors {
                log_line(&log, e);
            }
        }
        pane.wait_for_delivery();
        // Every byte the child wrote is in the ring now: persist the final tail
        // BEFORE the reap, which can take a while (a scope's reap passes) and
        // is a window in which this holder could itself be killed. It closes
        // the writer under its mutex, and the lock is still held, so no later
        // timer tick can touch a successor holder's files.
        crate::scrollback::persist_final(&pane, &scrollback, &log);
        // Plan D8: on a KILL this holder is the reaper of its own child tree.
        // Still under the lock, so a boot sweep cannot take this pane for dead
        // while its remainder is being ended.
        let mut reap = pane.reap_tree();
        // The pane-end report carries what failed above, too.
        for e in end_errors {
            reap.note(e);
        }
        if !reap.killed.is_empty() || !reap.notes.is_empty() {
            log_line(&log, &format!("pane ended; its tree: {}", reap.summary()));
        }
        drop(lock);
        exit
    }
}

/// The transient scope unit this holder runs in, when the spawner put it in
/// one (`crate::spawn`'s scope route). Recorded in the lock so a boot sweep can
/// stop what a dead holder left in it.
pub(crate) fn own_scope_unit() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let cgroup = crate::reap::own_holder_scope_cgroup()?;
        cgroup.rsplit('/').next().map(str::to_string)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Run [`accept_loop`] on a thread of its own. A holder upgrade's predecessor
/// that takes its pane back after a dead successor replaced its socket starts
/// a second one on a fresh listener (plan Phase 8).
pub(crate) fn spawn_accept_loop(
    listener: Listener,
    info: Arc<HolderInfo>,
    pane: Arc<Pane>,
    log: PathBuf,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("pty-holder-accept".into())
        .spawn(move || accept_loop(&listener, &info, &pane, &log))
        .map(|_| ())
}

/// Accept forever, one thread per connection, at most [`MAX_CONNECTIONS`] at
/// once. An accept error NEVER ends the holder — it holds a live pane, and an
/// endpoint problem is not a reason to end a session (Phase 1 hand-off). A
/// transient one backs off from [`ACCEPT_BACKOFF_MIN`] to
/// [`ACCEPT_BACKOFF_MAX`]; one the transport calls fatal (Windows: the pipe
/// name squatted, or an instance stuck for
/// `transport::windows::STUCK_INSTANCE_FATAL_AFTER`) is logged and retried
/// every [`ACCEPT_BACKOFF_FATAL`].
fn accept_loop(listener: &Listener, info: &Arc<HolderInfo>, pane: &Arc<Pane>, log: &Path) {
    let active = Arc::new(AtomicUsize::new(0));
    let mut backoff = ACCEPT_BACKOFF_MIN;
    loop {
        let conn = match listener.accept() {
            Ok(c) => {
                backoff = ACCEPT_BACKOFF_MIN;
                c
            }
            Err(e) if transport::accept_error_is_fatal(&e) => {
                log_line(log, &format!("accept (fatal, retrying): {e}"));
                std::thread::sleep(ACCEPT_BACKOFF_FATAL);
                continue;
            }
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let mut conn = conn;
            reject_conn(
                &mut conn,
                RejectReason::Busy,
                "holder is at its connection cap",
            );
            continue;
        }
        let info = Arc::clone(info);
        let pane = Arc::clone(pane);
        let slot = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("pty-holder-conn".into())
            .spawn(move || {
                serve_conn(conn, &info, Some(&pane));
                slot.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            // The closure (and its conn) was dropped, so the client sees
            // EOF; give the slot back.
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Where a connection is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    AwaitHello,
    /// Handshake done at this version.
    Negotiated(u32),
    /// Handshake done, no common version: envelope verbs only.
    EnvelopeOnly,
    /// Version >= 2 and `attach` answered: output is streaming.
    Attached(u32),
    /// Version >= 2 and `open_input` answered: the pane's input stream. The
    /// ONLY state that accepts input frames, so the only dispatch that can
    /// ever wait on the pane's input queue.
    InputOnly(u32),
}

/// What the dispatcher decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Send this reply and keep serving.
    Reply(Reply),
    /// Send `rejected` and close.
    Reject(RejectReason, String),
    /// Start streaming from `from` (`None`: a fresh attach); `restore` asks
    /// for the envelope, and is only ever true at version 3 or later.
    Attach {
        from: Option<u64>,
        restore: bool,
    },
    /// Write these bytes to the child.
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Set this connection's pause flag.
    Flow {
        paused: bool,
    },
    Kill,
    Detach,
    /// Hand the pane to a successor holder (plan Phase 8, Unix).
    #[cfg(unix)]
    PrepareUpgrade,
}

/// One connection's flow flags, shared with its pump.
#[derive(Debug, Default)]
struct ConnFlow {
    paused: AtomicBool,
    closed: AtomicBool,
}

/// Authorize the peer, then dispatch frames until EOF, error, rejection or
/// `detach`. `pane` is `None` only in unit tests of the dispatch.
pub fn serve_conn(conn: Conn, info: &HolderInfo, pane: Option<&Arc<Pane>>) {
    serve_conn_with(
        conn,
        info,
        pane,
        ConnParams {
            handshake_timeout: HANDSHAKE_TIMEOUT,
            input_queue_timeout: INPUT_QUEUE_TIMEOUT,
            #[cfg(unix)]
            expected_uid: transport::own_uid(),
        },
    );
}

/// The knobs of [`serve_conn`], separated so tests can shorten the handshake
/// timeout and force a peer-uid mismatch without a second OS user.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnParams {
    /// The whole-handshake bound: the FIRST frame must arrive complete within
    /// it, however it is trickled.
    pub handshake_timeout: Duration,
    /// Longest one input frame waits for room in the pane's input queue
    /// before the connection is closed. See [`INPUT_QUEUE_TIMEOUT`].
    pub input_queue_timeout: Duration,
    /// The uid a peer must have.
    #[cfg(unix)]
    pub expected_uid: u32,
}

/// Write one frame on the shared write half, bounded by `timeout`.
fn send_frame(writer: &Mutex<Conn>, kind: u8, payload: &[u8], timeout: Duration) -> io::Result<()> {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    let deadline = Instant::now() + timeout;
    write_frame(&mut DeadlineIo::new(&mut w, deadline), kind, payload)
}

fn send_reply(writer: &Mutex<Conn>, reply: &Reply) -> io::Result<()> {
    // An unserializable reply closes the connection rather than leaving the
    // client waiting for nothing.
    let payload = to_payload(reply).map_err(io::Error::other)?;
    send_frame(writer, KIND_CONTROL, &payload, REPLY_WRITE_TIMEOUT)
}

pub(crate) fn serve_conn_with(
    mut conn: Conn,
    info: &HolderInfo,
    pane: Option<&Arc<Pane>>,
    params: ConnParams,
) {
    #[cfg(unix)]
    {
        match conn.peer_uid() {
            Ok(uid) if transport::peer_authorized(uid, params.expected_uid) => {}
            Ok(uid) => {
                reject_conn(
                    &mut conn,
                    RejectReason::PeerNotAuthorized,
                    &format!("peer uid {uid} is not the holder's user"),
                );
                return;
            }
            Err(e) => {
                reject_conn(
                    &mut conn,
                    RejectReason::PeerNotAuthorized,
                    &format!("peer credentials unavailable: {e}"),
                );
                return;
            }
        }
    }
    // Windows: the pipe's DACL already refused every other user at open time.

    // The write half: every reply and every output frame goes through it.
    let writer = match conn.try_clone() {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(_) => return,
    };
    let flow = Arc::new(ConnFlow::default());

    let handshake_deadline = Instant::now() + params.handshake_timeout;
    let mut state = ConnState::AwaitHello;
    // This connection's input generation, once it became the input stream.
    let mut input_gen: Option<u64> = None;
    // Unix: this connection's registration as an input stream, so a
    // committing holder upgrade can drain it (`Pane::drain_input_for_handoff`).
    #[cfg(unix)]
    let mut input_reg: Option<u64> = None;
    loop {
        let read = if state == ConnState::AwaitHello {
            // One deadline for the whole first frame (DeadlineIo re-arms it
            // before every read), so a trickling client cannot hold a thread.
            read_frame(&mut DeadlineIo::new(&mut conn, handshake_deadline))
        } else {
            // Handshake done: an idle runner connection is legitimate.
            let _ = conn.set_read_timeout(None);
            read_frame(&mut conn)
        };
        let frame = match read {
            Ok(Some(f)) => f,
            // EOF, timeout, truncated or oversized frame: nothing more to say.
            Ok(None) | Err(_) => break,
        };
        let action = match frame.kind {
            KIND_CONTROL => match parse_request(&frame.payload) {
                Ok(req) => handle_request(&mut state, req, info),
                Err((reason, detail)) => Action::Reject(reason, detail),
            },
            KIND_DATA => handle_data(state, frame.payload),
            other => Action::Reject(
                RejectReason::UnknownFrameKind,
                format!("frame kind 0x{other:02x} is not accepted"),
            ),
        };
        // Every action that needs the pane, in a holder that has none, is
        // refused the same way (only the dispatch unit tests run without one).
        let needs_pane = matches!(
            action,
            Action::Attach { .. } | Action::Input(_) | Action::Resize { .. } | Action::Kill
        );
        let pane = match (needs_pane, pane) {
            (true, None) => {
                reject(
                    &writer,
                    RejectReason::VerbNotInVersion,
                    "no pane in this holder",
                );
                break;
            }
            (_, p) => p,
        };
        match action {
            Action::Reply(reply) => {
                // `open_input` just made this connection the input stream:
                // supersede every older input connection BEFORE answering, so
                // nothing the client sends after the `ok` can be overtaken by
                // an older connection's input.
                if matches!(state, ConnState::InputOnly(_)) && input_gen.is_none() {
                    #[cfg(unix)]
                    if let Some(p) = pane {
                        // Refused while an upgrade commits: no `ok`, so the
                        // client sends nothing here and reconnects to the
                        // successor.
                        match conn.try_clone().ok().and_then(|c| p.register_input_conn(c)) {
                            Some(id) => input_reg = Some(id),
                            None => {
                                reject(
                                    &writer,
                                    RejectReason::Busy,
                                    "this holder is handing its pane to a successor",
                                );
                                break;
                            }
                        }
                    }
                    input_gen = pane.map(|p| p.supersede_input());
                }
                if send_reply(&writer, &reply).is_err() {
                    break;
                }
            }
            Action::Reject(reason, detail) => {
                reject(&writer, reason, &detail);
                break;
            }
            Action::Attach { from, restore } => {
                let Some(pane) = pane else { break };
                let ConnState::Negotiated(v) = state else {
                    break;
                };
                if start_attach(pane, &writer, &flow, from, restore).is_err() {
                    break;
                }
                state = ConnState::Attached(v);
            }
            Action::Input(bytes) => {
                // No reply for input. Only an `open_input` connection gets
                // here (`handle_data`), so waiting on a full input queue
                // back-pressures that socket alone; the attached connection's
                // dispatch never runs this arm. A child that is gone takes
                // nothing; that is not this connection's error. A queue that
                // stays full for `params.input_queue_timeout` closes THIS input
                // connection — its thread and slot are not held hostage by a
                // child that ignores stdin; the runner reopens one.
                // A SUPERSEDED connection (a newer `open_input` exists) has
                // its input discarded and is closed: its bytes must never
                // follow the newer connection's.
                if let (Some(pane), Some(generation)) = (pane, input_gen) {
                    match pane.write_input(&bytes, params.input_queue_timeout, generation) {
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::TimedOut | io::ErrorKind::Other
                            ) =>
                        {
                            break
                        }
                        _ => {}
                    }
                }
            }
            Action::Resize { cols, rows } => {
                if cols == 0 || rows == 0 {
                    reject(
                        &writer,
                        RejectReason::Malformed,
                        "resize to a zero dimension",
                    );
                    break;
                }
                // `ok` means accepted: a PTY whose child is gone ignores it.
                if let Some(pane) = pane {
                    let _ = pane.resize(cols, rows);
                }
                if send_reply(&writer, &ok("resize")).is_err() {
                    break;
                }
            }
            Action::Flow { paused } => {
                flow.paused.store(paused, Ordering::SeqCst);
                if let Some(p) = pane {
                    p.notify();
                }
                let verb = if paused { "pause" } else { "resume" };
                if send_reply(&writer, &ok(verb)).is_err() {
                    break;
                }
            }
            Action::Kill => {
                if send_reply(&writer, &ok("kill")).is_err() {
                    break;
                }
                if let Some(pane) = pane {
                    pane.kill();
                }
            }
            Action::Detach => {
                let _ = send_reply(&writer, &ok("detach"));
                break;
            }
            #[cfg(unix)]
            Action::PrepareUpgrade => {
                let reply = match (pane, &info.upgrade) {
                    (Some(pane), Some(ctx)) => {
                        let result = crate::handoff::prepare_upgrade(ctx, info, pane);
                        let reply = result.reply();
                        if matches!(result, crate::handoff::UpgradeResult::Committed { .. }) {
                            // The successor owns the pane: tell the requester,
                            // then leave at once (no pane-end work, no
                            // destructor — `handoff::exit_after_handoff`).
                            let _ = send_reply(&writer, &reply);
                            crate::handoff::exit_after_handoff();
                        }
                        reply
                    }
                    _ => Reply::Unsupported {
                        verb: "prepare_upgrade".into(),
                        reason: "this holder has no pane to hand over".into(),
                    },
                };
                if send_reply(&writer, &reply).is_err() {
                    break;
                }
            }
        }
    }
    // Teardown: stop this connection's pump (if any) and close both halves.
    #[cfg(unix)]
    if let (Some(p), Some(id)) = (pane, input_reg) {
        p.unregister_input_conn(id);
    }
    flow.closed.store(true, Ordering::SeqCst);
    if let Some(p) = pane {
        p.notify();
    }
    writer.lock().unwrap_or_else(|p| p.into_inner()).shutdown();
}

fn ok(verb: &str) -> Reply {
    Reply::Ok {
        verb: verb.to_string(),
    }
}

/// Answer `attach` and start this connection's pump. The `attached` reply —
/// and, for a restoring attach, the preamble — is written BEFORE the pump
/// exists, so they are always the first frames of the stream.
fn start_attach(
    pane: &Arc<Pane>,
    writer: &Arc<Mutex<Conn>>,
    flow: &Arc<ConnFlow>,
    from: Option<u64>,
    restore: bool,
) -> io::Result<()> {
    let reply = {
        let st = pane.lock_state();
        let ring_start = st.ring.start_offset();
        let end = st.ring.end_offset();
        let start = match from {
            None => end.saturating_sub(ATTACH_TAIL_BYTES).max(ring_start),
            Some(x) => x.min(end),
        };
        let meta = restore.then(|| {
            crate::metadata::restore_meta(st.modes.cwd(), pane.spawn_cwd(), st.modes.title())
        });
        AttachedReply {
            start_offset: start,
            ring_start_offset: ring_start,
            end_offset: end,
            child_pid: pane.child_pid(),
            cols: st.size.0,
            rows: st.size.1,
            restore: meta,
        }
    };
    let next = reply.start_offset;
    send_reply(writer, &Reply::Attached(reply))?;
    let (pane, writer, flow) = (Arc::clone(pane), Arc::clone(writer), Arc::clone(flow));
    std::thread::Builder::new()
        .name("pty-holder-pump".into())
        .spawn(move || pump(&pane, &writer, &flow, next, restore))?;
    Ok(())
}

fn send_synthetic(
    writer: &Mutex<Conn>,
    part: SyntheticPart,
    anchor: u64,
    bytes: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    send_frame(
        writer,
        KIND_SYNTHETIC,
        &synthetic_payload(part, anchor, bytes),
        timeout,
    )
}

/// What a pump does next, decided under the pane lock by [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Stop,
    /// Send the held postamble now.
    Postamble,
    Lost {
        from: u64,
        to: u64,
    },
    /// Send up to `max` ring bytes from `from`.
    Output {
        from: u64,
        max: usize,
    },
    Exit,
    Wait,
}

/// Everything [`decide`] reads, as one value so the rule is testable without
/// a pane.
#[derive(Debug, Clone, Copy)]
struct PumpView {
    closed: bool,
    paused: bool,
    /// The connection's next stream offset.
    next: u64,
    ring_start: u64,
    ring_end: u64,
    /// The anchor of a postamble still to be sent.
    postamble_at: Option<u64>,
    exited: bool,
}

/// The pump's rule. The postamble goes out the moment the stream reaches its
/// anchor — and also BEFORE a loss report that would carry the stream past
/// it: sent after, it would land behind bytes newer than the state it
/// describes (a stale `?2004h` after the pane's own later `?2004l`). Output is
/// cut at a pending anchor, so nothing produced after the attach precedes the
/// postamble. The postamble and the `exit` are not output: a paused connection
/// still gets them, and the postamble always precedes the `exit`.
fn decide(v: PumpView) -> Decision {
    if v.closed {
        return Decision::Stop;
    }
    if let Some(at) = v.postamble_at {
        let reached = v.next >= at;
        let loss_crosses = !v.paused && v.next < v.ring_start && at <= v.ring_start;
        if reached || loss_crosses {
            return Decision::Postamble;
        }
    }
    if !v.paused {
        if v.next < v.ring_start {
            return Decision::Lost {
                from: v.next,
                to: v.ring_start,
            };
        }
        if v.next < v.ring_end {
            let max = match v.postamble_at {
                Some(at) => MAX_OUTPUT_CHUNK.min((at - v.next) as usize),
                None => MAX_OUTPUT_CHUNK,
            };
            return Decision::Output { from: v.next, max };
        }
    }
    if v.exited && v.next >= v.ring_end {
        return Decision::Exit;
    }
    Decision::Wait
}

/// What a pump sends next.
enum PumpStep {
    Postamble,
    Lost { from: u64, to: u64 },
    Output { from: u64, bytes: Vec<u8> },
    Exit(ExitReply),
    Stop,
}

/// Stream the pane's output to one connection from absolute offset `next`.
///
/// With `restore` (a version-3 `attach {restore: true}`), the pump first
/// reports any loss already known (the requested offset left the ring), then
/// sends the PREAMBLE and holds the POSTAMBLE, both from one look at the mode
/// tracker under the pane lock — the preamble for the first byte this
/// connection can still be sent (`max(next, ring start)`), the postamble
/// anchored at the stream's end at that moment. Both frames are ALWAYS sent
/// when asked for, empty or not, so a client can tell "delivered" from "lost
/// with the connection". The ring may roll between that look and the
/// preamble's write (the lock is not held across I/O); the pump then reports
/// the rolled-past bytes as `output_lost` after the preamble, exactly as for a
/// client too slow to keep up — the preamble still names the position the
/// stream resumed from, never a byte the client is later sent as if it were
/// the first.
fn pump(pane: &Arc<Pane>, writer: &Mutex<Conn>, flow: &ConnFlow, mut next: u64, restore: bool) {
    let mut postamble: Option<(u64, Vec<u8>)> = None;
    if restore {
        let (lost, env) = {
            let st = pane.lock_state();
            let ring_start = st.ring.start_offset();
            let lost = (next < ring_start).then_some((next, ring_start));
            (lost, st.modes.envelope(next.max(ring_start)))
        };
        // A loss already known now is reported BEFORE the preamble, so a
        // client reads "continuity broken, then the state where the stream
        // resumes" — and knows that preamble is one it needs.
        let lost_sent = match lost {
            Some((from, to)) => {
                next = to;
                to_payload(&Reply::OutputLost {
                    from_offset: from,
                    to_offset: to,
                })
                .map_err(io::Error::other)
                .and_then(|p| send_frame(writer, KIND_CONTROL, &p, PUMP_WRITE_TIMEOUT))
            }
            None => Ok(()),
        };
        let sent = lost_sent.and_then(|()| {
            send_synthetic(
                writer,
                SyntheticPart::Preamble,
                env.preamble_at,
                &env.preamble,
                PUMP_WRITE_TIMEOUT,
            )
        });
        if sent.is_err() {
            flow.closed.store(true, Ordering::SeqCst);
            writer.lock().unwrap_or_else(|p| p.into_inner()).shutdown();
            return;
        }
        postamble = Some((env.postamble_at, env.postamble));
    }
    loop {
        let step = {
            let mut st = pane.lock_state();
            loop {
                let view = PumpView {
                    closed: flow.closed.load(Ordering::SeqCst),
                    paused: flow.paused.load(Ordering::SeqCst),
                    next,
                    ring_start: st.ring.start_offset(),
                    ring_end: st.ring.end_offset(),
                    postamble_at: postamble.as_ref().map(|(at, _)| *at),
                    exited: st.exit.is_some(),
                };
                match decide(view) {
                    Decision::Stop => break PumpStep::Stop,
                    Decision::Postamble => break PumpStep::Postamble,
                    Decision::Lost { from, to } => break PumpStep::Lost { from, to },
                    Decision::Output { from, max } => {
                        let (from, bytes) = st.ring.slice_from(from, max);
                        break PumpStep::Output { from, bytes };
                    }
                    Decision::Exit => {
                        if let Some(exit) = st.exit {
                            break PumpStep::Exit(exit);
                        }
                    }
                    Decision::Wait => {}
                }
                st = pane.wait_changed(st, PUMP_POLL);
            }
        };
        let sent = match step {
            PumpStep::Stop => return,
            PumpStep::Postamble => match postamble.take() {
                Some((at, bytes)) => send_synthetic(
                    writer,
                    SyntheticPart::Postamble,
                    at,
                    &bytes,
                    PUMP_WRITE_TIMEOUT,
                ),
                None => Ok(()),
            },
            PumpStep::Lost { from, to } => {
                next = to;
                to_payload(&Reply::OutputLost {
                    from_offset: from,
                    to_offset: to,
                })
                .map_err(io::Error::other)
                .and_then(|p| send_frame(writer, KIND_CONTROL, &p, PUMP_WRITE_TIMEOUT))
            }
            PumpStep::Output { from, bytes } => {
                next = from + bytes.len() as u64;
                send_frame(
                    writer,
                    KIND_OUTPUT,
                    &output_payload(from, &bytes),
                    PUMP_WRITE_TIMEOUT,
                )
            }
            PumpStep::Exit(exit) => {
                // The flush (Windows: wait for the peer to read the frame)
                // runs on a handle of the pump's own, OUTSIDE the writer lock:
                // a runner that keeps the pipe open but stops reading must pin
                // this pump at most, never a dispatch reply or the teardown.
                let sent = to_payload(&Reply::Exit(exit))
                    .map_err(io::Error::other)
                    .and_then(|p| send_frame(writer, KIND_CONTROL, &p, PUMP_WRITE_TIMEOUT))
                    .and_then(|()| writer.lock().unwrap_or_else(|p| p.into_inner()).try_clone())
                    .and_then(|own| own.flush_to_peer());
                if sent.is_ok() {
                    pane.mark_exit_delivered();
                }
                return;
            }
        };
        if sent.is_err() {
            // The client stopped reading (or left): drop the connection; it
            // reattaches at its own last offset.
            flow.closed.store(true, Ordering::SeqCst);
            writer.lock().unwrap_or_else(|p| p.into_inner()).shutdown();
            return;
        }
    }
}

/// The allowlist for CONTROL frames. Every arm is a verb this build implements
/// in that state; the final arm is the refusal.
fn handle_request(state: &mut ConnState, req: Request, info: &HolderInfo) -> Action {
    use ConnState::{Attached, AwaitHello, EnvelopeOnly, InputOnly, Negotiated};
    match (*state, req) {
        (AwaitHello, Request::Hello { versions }) => {
            match negotiate(&versions, PROTOCOL_VERSIONS) {
                Some(version) => {
                    *state = Negotiated(version);
                    Action::Reply(Reply::HelloAck(HelloAck {
                        version,
                        holder_build: holder_build(),
                        holder_pid: info.holder_pid,
                        child_pid: info.child_pid,
                    }))
                }
                None => {
                    *state = EnvelopeOnly;
                    Action::Reply(Reply::NoCommonVersion {
                        holder_versions: PROTOCOL_VERSIONS.to_vec(),
                    })
                }
            }
        }
        (AwaitHello, other) => Action::Reject(
            RejectReason::HandshakeRequired,
            format!("{:?} before hello", other.verb()),
        ),
        (_, Request::Hello { .. }) => Action::Reject(
            RejectReason::DuplicateHello,
            "hello already answered on this connection".into(),
        ),
        // Envelope verbs: answerable whatever was negotiated (D15).
        (_, Request::Census) => Action::Reply(Reply::CensusReply(CensusReply {
            pane_id: info.pane_id.as_str().to_string(),
            holder_pid: info.holder_pid,
            child_pid: info.child_pid,
            holder_build: holder_build(),
            versions: PROTOCOL_VERSIONS.to_vec(),
            started_at_unix_ms: info.started_at_unix_ms,
        })),
        // Envelope verb (D15): answerable whatever was negotiated, so a runner
        // far ahead of this holder can still replace it.
        #[cfg(unix)]
        (_, Request::PrepareUpgrade) => Action::PrepareUpgrade,
        #[cfg(not(unix))]
        (_, Request::PrepareUpgrade) => Action::Reply(Reply::Unsupported {
            verb: "prepare_upgrade".into(),
            reason: UPGRADE_UNSUPPORTED_ON_THIS_OS.into(),
        }),
        // Version 1 verbs.
        (Negotiated(v) | Attached(v) | InputOnly(v), Request::Ping) if v >= 1 => {
            Action::Reply(Reply::Pong)
        }
        // Version 2 verbs: the data path.
        (
            Negotiated(v),
            Request::Attach {
                from_offset,
                restore,
            },
        ) if v >= DATA_PATH_VERSION => Action::Attach {
            from: from_offset,
            // A version-2 connection never gets the envelope, whatever a
            // confused client put in the field.
            restore: restore && v >= RESTORE_VERSION,
        },
        (Attached(_), Request::Attach { .. } | Request::OpenInput) => Action::Reject(
            RejectReason::AlreadyAttached,
            "this connection is already attached".into(),
        ),
        (Negotiated(v), Request::OpenInput) if v >= DATA_PATH_VERSION => {
            *state = InputOnly(v);
            Action::Reply(Reply::Ok {
                verb: "open_input".into(),
            })
        }
        (InputOnly(_), Request::Attach { .. } | Request::OpenInput) => Action::Reject(
            RejectReason::AlreadyAttached,
            "this connection is an input stream".into(),
        ),
        (Negotiated(v) | Attached(v) | InputOnly(v), Request::Resize { cols, rows })
            if v >= DATA_PATH_VERSION =>
        {
            Action::Resize { cols, rows }
        }
        (Attached(_), Request::Pause) => Action::Flow { paused: true },
        (Attached(_), Request::Resume) => Action::Flow { paused: false },
        (Negotiated(v) | InputOnly(v), req @ (Request::Pause | Request::Resume))
            if v >= DATA_PATH_VERSION =>
        {
            Action::Reject(
                RejectReason::NotAttached,
                format!("{:?} needs an attached connection", req.verb()),
            )
        }
        (Negotiated(v) | Attached(v) | InputOnly(v), Request::Kill) if v >= DATA_PATH_VERSION => {
            Action::Kill
        }
        (Negotiated(v) | Attached(v) | InputOnly(v), Request::Detach) if v >= DATA_PATH_VERSION => {
            Action::Detach
        }
        (_, other) => Action::Reject(
            RejectReason::VerbNotInVersion,
            format!(
                "{:?} is not available at this connection's negotiated version",
                other.verb()
            ),
        ),
    }
}

/// The allowlist for DATA frames: the pane's input, on an `open_input`
/// connection only — never on the attached one, whose dispatch must stay free
/// for `resize`/`pause`/`resume`/`detach` however slowly the child reads.
fn handle_data(state: ConnState, payload: Vec<u8>) -> Action {
    match state {
        ConnState::InputOnly(_) => Action::Input(payload),
        ConnState::Attached(_) => Action::Reject(
            RejectReason::UnexpectedDataFrame,
            "input goes on an `open_input` connection, never on the attached one".into(),
        ),
        ConnState::Negotiated(v) if v >= DATA_PATH_VERSION => Action::Reject(
            RejectReason::NotAttached,
            "input frames are accepted on an `open_input` connection only".into(),
        ),
        _ => Action::Reject(
            RejectReason::UnexpectedDataFrame,
            "no data frames before a version-2 attach".into(),
        ),
    }
}

/// Send `rejected` (best effort, bounded) on an unsplit connection and close.
fn reject_conn(conn: &mut Conn, reason: RejectReason, detail: &str) {
    let reply = Reply::Rejected {
        reason,
        detail: detail.to_string(),
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    // A reason that cannot be serialized (only the deserialize-only Unknown)
    // still closes the connection; it just closes without the frame.
    if let Ok(payload) = to_payload(&reply) {
        let _ = write_frame(&mut DeadlineIo::new(conn, deadline), KIND_CONTROL, &payload);
    }
    conn.shutdown();
}

/// [`reject_conn`] on the shared write half.
fn reject(writer: &Mutex<Conn>, reason: RejectReason, detail: &str) {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    reject_conn(&mut w, reason, detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> HolderInfo {
        HolderInfo {
            pane_id: PaneId::new("p").unwrap(),
            holder_pid: 7,
            child_pid: None,
            started_at_unix_ms: 1,
            #[cfg(unix)]
            upgrade: None,
        }
    }

    #[test]
    fn pty_holder_dispatch_state_machine() {
        let i = info();
        let mut s = ConnState::AwaitHello;
        assert!(matches!(
            handle_request(&mut s, Request::Ping, &i),
            Action::Reject(RejectReason::HandshakeRequired, _)
        ));
        let mut s = ConnState::AwaitHello;
        assert!(matches!(
            handle_request(&mut s, Request::Census, &i),
            Action::Reject(RejectReason::HandshakeRequired, _)
        ));

        let mut s = ConnState::AwaitHello;
        match handle_request(
            &mut s,
            Request::Hello {
                versions: vec![1, 99],
            },
            &i,
        ) {
            Action::Reply(Reply::HelloAck(a)) => {
                assert_eq!(a.version, 1);
                assert_eq!(a.holder_pid, 7);
                assert_eq!(a.child_pid, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s, ConnState::Negotiated(1));
        assert_eq!(
            handle_request(&mut s, Request::Ping, &i),
            Action::Reply(Reply::Pong)
        );
        assert!(matches!(
            handle_request(&mut s, Request::Census, &i),
            Action::Reply(Reply::CensusReply(_))
        ));
        // Unix: a handoff action (served as `unsupported` by a holder with no
        // pane); elsewhere `unsupported` straight away (plan D12).
        #[cfg(unix)]
        assert_eq!(
            handle_request(&mut s, Request::PrepareUpgrade, &i),
            Action::PrepareUpgrade
        );
        #[cfg(not(unix))]
        assert!(matches!(
            handle_request(&mut s, Request::PrepareUpgrade, &i),
            Action::Reply(Reply::Unsupported { .. })
        ));
        assert!(matches!(
            handle_request(&mut s, Request::Hello { versions: vec![1] }, &i),
            Action::Reject(RejectReason::DuplicateHello, _)
        ));

        let mut s = ConnState::AwaitHello;
        assert_eq!(
            handle_request(&mut s, Request::Hello { versions: vec![99] }, &i),
            Action::Reply(Reply::NoCommonVersion {
                holder_versions: PROTOCOL_VERSIONS.to_vec()
            })
        );
        assert_eq!(s, ConnState::EnvelopeOnly);
        // Envelope verbs survive a failed negotiation; version-scoped ones do not.
        assert!(matches!(
            handle_request(&mut s, Request::Census, &i),
            Action::Reply(Reply::CensusReply(_))
        ));
        assert!(matches!(
            handle_request(&mut s, Request::Ping, &i),
            Action::Reject(RejectReason::VerbNotInVersion, _)
        ));
    }

    /// Drive `serve_conn_with` on one end of a socketpair; returns the client
    /// end and the server thread.
    #[cfg(unix)]
    fn serve_pair(
        params: ConnParams,
    ) -> (std::os::unix::net::UnixStream, std::thread::JoinHandle<()>) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            serve_conn_with(Conn::from_stream(server), &info(), None, params)
        });
        (client, t)
    }

    /// HANDSHAKE_TIMEOUT: a connection that never completes its first frame
    /// is dropped — both a silent one and one that trickles a byte at a time
    /// (the bound is on the whole frame, not on each read).
    #[cfg(unix)]
    #[test]
    fn pty_holder_handshake_timeout_drops_idle_and_trickling_clients() {
        use std::io::{Read, Write};
        // What this pins is that the server ENDS the connection by its own
        // deadline, not how promptly a loaded CI runner schedules it. So
        // every upper bound below is several timeouts wide, but each stays
        // below what the defect it guards against would take (findings
        // 53528293 and 338f66d9: the old 3x bound flaked on a busy runner).
        let timeout = Duration::from_millis(300);
        let params = ConnParams {
            handshake_timeout: timeout,
            input_queue_timeout: INPUT_QUEUE_TIMEOUT,
            expected_uid: transport::own_uid(),
        };

        // `started` precedes the spawn, so the server's own deadline (stamped
        // inside its thread) is at least `started + timeout`: the lower bound
        // below holds by construction, whatever the scheduler does.
        let started = Instant::now();
        let (mut client, t) = serve_pair(params);
        // Bounded: a server that never closes fails the test, not hangs it.
        client.set_read_timeout(Some(timeout * 20)).unwrap();
        let mut buf = [0u8; 8];
        // The server closes: EOF, with nothing sent.
        assert_eq!(client.read(&mut buf).unwrap(), 0);
        t.join().unwrap();
        let took = started.elapsed();
        assert!(took >= timeout && took < timeout * 10, "idle: {took:?}");

        let (client, t) = serve_pair(params);
        // A hello prefix, one byte every 100 ms: each byte lands inside a
        // per-read timeout, so only a whole-frame deadline can stop it.
        let hello = crate::frame::encode_frame(
            KIND_CONTROL,
            &to_payload(&Request::Hello { versions: vec![1] }).unwrap(),
        )
        .unwrap();
        let step = Duration::from_millis(100);
        let bound = timeout * 7;
        // Self-check: a server with only a per-read timeout would hold the
        // thread for the whole trickle, which must outlast the bound by a
        // clear margin or this half could not tell the two servers apart.
        assert!(step * (hello.len() as u32) > bound + timeout * 3);
        let started = Instant::now();
        // The trickle runs on its own thread so that `took` measures how
        // long the SERVER held the connection, not how long the client
        // kept writing into a socket the server already closed.
        let writer = std::thread::spawn(move || {
            let mut client = client;
            for b in hello {
                if client.write_all(&[b]).is_err() {
                    break;
                }
                std::thread::sleep(step);
            }
        });
        t.join().unwrap();
        let took = started.elapsed();
        writer.join().unwrap();
        assert!(took < bound, "trickle held the thread: {took:?}");
    }

    /// Peer-uid rejection, wired end to end through `serve_conn_with`.
    ///
    /// The kernel half (SO_PEERCRED reporting a DIFFERENT uid) cannot be
    /// exercised without root: an unprivileged process can only connect as its
    /// own uid, and a user namespace does not help — SO_PEERCRED reports the
    /// peer's credentials mapped into the READER's namespace, which for an
    /// unprivileged userns is this same uid. So the test forces the mismatch
    /// from the other side, by expecting a uid the peer does not have.
    #[cfg(unix)]
    #[test]
    fn pty_holder_peer_uid_mismatch_is_rejected() {
        use std::io::Read;
        let params = ConnParams {
            handshake_timeout: Duration::from_secs(5),
            input_queue_timeout: INPUT_QUEUE_TIMEOUT,
            expected_uid: transport::own_uid().wrapping_add(1),
        };
        let (mut client, t) = serve_pair(params);
        client
            .set_read_timeout(Some(Duration::from_secs(5) * 5))
            .unwrap();
        let frame = read_frame(&mut client).unwrap().expect("a rejection");
        match crate::protocol::parse_reply(&frame.payload).unwrap() {
            Reply::Rejected { reason, .. } => assert_eq!(reason, RejectReason::PeerNotAuthorized),
            other => panic!("{other:?}"),
        }
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "closed after the rejection");
        t.join().unwrap();
    }

    /// The version-2 data-path allowlist: attach only once and only at v2;
    /// attached-only verbs and input frames refused before attach; the v2
    /// verbs refused on a v1 connection.
    #[test]
    fn pty_holder_dispatch_v2_data_path_allowlist() {
        let i = info();
        let mut s = ConnState::AwaitHello;
        handle_request(
            &mut s,
            Request::Hello {
                versions: vec![1, 2],
            },
            &i,
        );
        assert_eq!(s, ConnState::Negotiated(2));
        // Before attach: input and flow are refused as not-attached.
        assert!(matches!(
            handle_data(s, vec![1]),
            Action::Reject(RejectReason::NotAttached, _)
        ));
        assert!(matches!(
            handle_request(&mut s, Request::Pause, &i),
            Action::Reject(RejectReason::NotAttached, _)
        ));
        // kill / resize / detach need no attach.
        assert_eq!(handle_request(&mut s, Request::Kill, &i), Action::Kill);
        assert_eq!(
            handle_request(&mut s, Request::Resize { cols: 9, rows: 3 }, &i),
            Action::Resize { cols: 9, rows: 3 }
        );
        assert_eq!(
            handle_request(
                &mut s,
                Request::Attach {
                    from_offset: Some(5),
                    restore: true,
                },
                &i
            ),
            Action::Attach {
                from: Some(5),
                restore: false,
            },
            "a version-2 connection never gets the restore envelope"
        );
        // The serve loop moves the state on a successful attach.
        let mut s = ConnState::Attached(2);
        // Input never rides the attached connection.
        assert!(matches!(
            handle_data(s, vec![0xFF]),
            Action::Reject(RejectReason::UnexpectedDataFrame, _)
        ));
        assert!(matches!(
            handle_request(&mut s, Request::OpenInput, &i),
            Action::Reject(RejectReason::AlreadyAttached, _)
        ));
        assert_eq!(
            handle_request(&mut s, Request::Pause, &i),
            Action::Flow { paused: true }
        );
        assert_eq!(
            handle_request(&mut s, Request::Resume, &i),
            Action::Flow { paused: false }
        );
        assert_eq!(
            handle_request(&mut s, Request::Ping, &i),
            Action::Reply(Reply::Pong)
        );
        assert!(matches!(
            handle_request(
                &mut s,
                Request::Attach {
                    from_offset: None,
                    restore: false,
                },
                &i
            ),
            Action::Reject(RejectReason::AlreadyAttached, _)
        ));
        assert_eq!(handle_request(&mut s, Request::Detach, &i), Action::Detach);

        // A version-1 connection gets none of it.
        let mut s = ConnState::AwaitHello;
        handle_request(&mut s, Request::Hello { versions: vec![1] }, &i);
        assert_eq!(s, ConnState::Negotiated(1));
        for req in [
            Request::Attach {
                from_offset: None,
                restore: false,
            },
            Request::Resize { cols: 1, rows: 1 },
            Request::Kill,
            Request::Detach,
            Request::Pause,
            Request::OpenInput,
        ] {
            assert!(
                matches!(
                    handle_request(&mut s, req.clone(), &i),
                    Action::Reject(RejectReason::VerbNotInVersion, _)
                ),
                "{req:?}"
            );
        }
        assert!(matches!(
            handle_data(s, vec![1]),
            Action::Reject(RejectReason::UnexpectedDataFrame, _)
        ));
        // And after no common version, nothing of the data path either.
        assert!(matches!(
            handle_data(ConnState::EnvelopeOnly, vec![1]),
            Action::Reject(RejectReason::UnexpectedDataFrame, _)
        ));
    }

    /// Review round 2, F2: input is accepted ONLY on an `open_input`
    /// connection. That connection takes input, ping, resize, kill and
    /// detach; it can never attach or pause; and nothing else takes input —
    /// so the attached connection's dispatch never waits on the input queue.
    #[test]
    fn pty_holder_dispatch_input_only_on_an_input_stream() {
        let i = info();
        let mut s = ConnState::AwaitHello;
        handle_request(
            &mut s,
            Request::Hello {
                versions: vec![1, 2],
            },
            &i,
        );
        assert!(matches!(
            handle_data(s, vec![1]),
            Action::Reject(RejectReason::NotAttached, _)
        ));
        assert_eq!(
            handle_request(&mut s, Request::OpenInput, &i),
            Action::Reply(Reply::Ok {
                verb: "open_input".into()
            })
        );
        assert_eq!(s, ConnState::InputOnly(2));
        assert_eq!(
            handle_data(s, vec![0x00, 0xFF]),
            Action::Input(vec![0x00, 0xFF])
        );
        assert_eq!(
            handle_request(&mut s, Request::Ping, &i),
            Action::Reply(Reply::Pong)
        );
        assert_eq!(handle_request(&mut s, Request::Kill, &i), Action::Kill);
        assert_eq!(
            handle_request(&mut s, Request::Resize { cols: 2, rows: 2 }, &i),
            Action::Resize { cols: 2, rows: 2 }
        );
        for req in [
            Request::Attach {
                from_offset: None,
                restore: false,
            },
            Request::OpenInput,
        ] {
            assert!(matches!(
                handle_request(&mut s, req, &i),
                Action::Reject(RejectReason::AlreadyAttached, _)
            ));
        }
        assert!(matches!(
            handle_request(&mut s, Request::Pause, &i),
            Action::Reject(RejectReason::NotAttached, _)
        ));
        assert_eq!(handle_request(&mut s, Request::Detach, &i), Action::Detach);
    }

    /// Version 3: `restore` reaches the attach action only on a connection
    /// that negotiated it.
    #[test]
    fn pty_holder_attach_restore_is_honoured_from_version_3() {
        let i = info();
        let mut s = ConnState::AwaitHello;
        handle_request(
            &mut s,
            Request::Hello {
                versions: vec![2, 3],
            },
            &i,
        );
        assert_eq!(s, ConnState::Negotiated(RESTORE_VERSION));
        assert_eq!(
            handle_request(
                &mut s,
                Request::Attach {
                    from_offset: None,
                    restore: true,
                },
                &i
            ),
            Action::Attach {
                from: None,
                restore: true,
            }
        );
    }

    fn view(next: u64, ring_start: u64, ring_end: u64, postamble_at: Option<u64>) -> PumpView {
        PumpView {
            closed: false,
            paused: false,
            next,
            ring_start,
            ring_end,
            postamble_at,
            exited: false,
        }
    }

    /// Review defect 3: a loss report that would carry the stream past the
    /// postamble's anchor is PRECEDED by the postamble — sent after, the stale
    /// attach-time state would land behind newer bytes. The old rule (send the
    /// postamble only once `next >= anchor`) answered `Lost` here.
    #[test]
    fn pty_holder_attach_postamble_precedes_a_loss_that_crosses_its_anchor() {
        assert_eq!(decide(view(10, 50, 100, Some(30))), Decision::Postamble);
        assert_eq!(decide(view(10, 50, 100, Some(50))), Decision::Postamble);
        // A loss that stops short of the anchor is reported first.
        assert_eq!(
            decide(view(10, 20, 100, Some(30))),
            Decision::Lost { from: 10, to: 20 }
        );
        // Output is cut at the anchor; at the anchor the postamble goes.
        assert_eq!(
            decide(view(25, 20, 100, Some(30))),
            Decision::Output { from: 25, max: 5 }
        );
        assert_eq!(decide(view(30, 20, 100, Some(30))), Decision::Postamble);
        // Paused: no loss is reported, so nothing crosses yet — but a reached
        // anchor still sends, and the postamble precedes the exit.
        let mut v = view(10, 50, 100, Some(30));
        v.paused = true;
        assert_eq!(decide(v), Decision::Wait);
        let mut v = view(100, 50, 100, Some(100));
        v.exited = true;
        assert_eq!(decide(v), Decision::Postamble);
        v.postamble_at = None;
        assert_eq!(decide(v), Decision::Exit);
    }

    #[test]
    fn pty_holder_start_refuses_a_relative_pane_dir() {
        let err =
            Holder::start(Path::new("relative/panes"), &PaneId::new("p").unwrap()).unwrap_err();
        assert!(matches!(err, StartError::PaneDir(_)), "{err}");
    }
}
