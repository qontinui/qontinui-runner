//! The holder side: lock, child, endpoint, and the per-connection dispatch.
//!
//! Start-up order is plan D3, per pane (D13), with Phase 2's child in it:
//!
//! 1. Make sure the pane directory exists and (Unix) is ours and 0700.
//! 2. Take `<pane-id>.lock`, non-blocking. Held → another holder is alive for
//!    this pane → exit, touching nothing (not even the spec).
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
//!   exit (`crate::pty::Pane::wait_for_end`).
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

use crate::frame::{output_payload, read_frame, write_frame, KIND_CONTROL, KIND_DATA, KIND_OUTPUT};
use crate::lock::{LockRecord, PaneLock, TryLock};
use crate::pane::{lock_path, log_path, prepare_private_dir, PaneId};
use crate::protocol::{
    holder_build, negotiate, parse_request, to_payload, AttachedReply, CensusReply, ExitReply,
    HelloAck, RejectReason, Reply, Request, ATTACH_TAIL_BYTES, DATA_PATH_VERSION,
    PROTOCOL_VERSIONS,
};
use crate::pty::Pane;
use crate::transport::{self, Conn, DeadlineIo, Listener};

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
        record: Option<LockRecord>,
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
}

/// A holder that holds its lock, owns its pane's child, and has bound its
/// endpoint.
#[derive(Debug)]
pub struct Holder {
    info: Arc<HolderInfo>,
    pane: Arc<Pane>,
    listener: Listener,
    log: PathBuf,
    // Held for the holder's lifetime; dropping it releases the pane.
    lock: PaneLock,
}

fn now_unix_ms() -> u64 {
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
        let mut lock = match PaneLock::try_acquire(&lock_file)? {
            TryLock::Acquired(l) => l,
            TryLock::Held => {
                return Err(StartError::LockHeld {
                    record: crate::lock::read_record(&lock_file),
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
        };
        let bound = (|| -> io::Result<Listener> {
            lock.write_record(&LockRecord {
                holder_pid: info.holder_pid,
                child_pid: info.child_pid,
                versions: PROTOCOL_VERSIONS.to_vec(),
                started_at_unix_ms: info.started_at_unix_ms,
                holder_build: holder_build(),
            })?;
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
        Ok(Holder {
            info: Arc::new(info),
            pane,
            listener,
            log: log_path(pane_dir, pane_id),
            lock,
        })
    }

    pub fn info(&self) -> &HolderInfo {
        &self.info
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

    /// Serve until the holder may exit (`crate::pty::Pane::wait_for_end`) and
    /// return the child's exit. The accept loop runs on its own thread and
    /// never ends the holder; the lock is held until this returns.
    pub fn run(self) -> ExitReply {
        let Holder {
            info,
            pane,
            listener,
            log,
            lock,
        } = self;
        let accept_pane = Arc::clone(&pane);
        let spawned = std::thread::Builder::new()
            .name("pty-holder-accept".into())
            .spawn(move || accept_loop(&listener, &info, &accept_pane, &log));
        if let Err(e) = spawned {
            // No accept loop: the pane runs on, unreachable but alive and
            // counted (its lock is held), until the child exits.
            eprintln!("pty-holder: accept thread: {e}");
        }
        let exit = pane.wait_for_end();
        drop(lock);
        exit
    }
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
    /// Start streaming from this offset (`None`: a fresh attach).
    Attach(Option<u64>),
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
            Action::Attach(_) | Action::Input(_) | Action::Resize { .. } | Action::Kill
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
            Action::Attach(from) => {
                let Some(pane) = pane else { break };
                let ConnState::Negotiated(v) = state else {
                    break;
                };
                if start_attach(pane, &writer, &flow, from).is_err() {
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
        }
    }
    // Teardown: stop this connection's pump (if any) and close both halves.
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

/// Answer `attach` and start this connection's pump. The `attached` reply is
/// written BEFORE the pump exists, so it is always the first frame of the
/// stream.
fn start_attach(
    pane: &Arc<Pane>,
    writer: &Arc<Mutex<Conn>>,
    flow: &Arc<ConnFlow>,
    from: Option<u64>,
) -> io::Result<()> {
    let reply = {
        let st = pane.lock_state();
        let ring_start = st.ring.start_offset();
        let end = st.ring.end_offset();
        let start = match from {
            None => end.saturating_sub(ATTACH_TAIL_BYTES).max(ring_start),
            Some(x) => x.min(end),
        };
        AttachedReply {
            start_offset: start,
            ring_start_offset: ring_start,
            end_offset: end,
            child_pid: pane.child_pid(),
            cols: st.size.0,
            rows: st.size.1,
        }
    };
    let next = reply.start_offset;
    send_reply(writer, &Reply::Attached(reply))?;
    let (pane, writer, flow) = (Arc::clone(pane), Arc::clone(writer), Arc::clone(flow));
    std::thread::Builder::new()
        .name("pty-holder-pump".into())
        .spawn(move || pump(&pane, &writer, &flow, next))?;
    Ok(())
}

/// What a pump does next.
enum PumpStep {
    Lost { from: u64, to: u64 },
    Output { from: u64, bytes: Vec<u8> },
    Exit(ExitReply),
    Stop,
}

/// Stream the pane's output to one connection from absolute offset `next`.
fn pump(pane: &Arc<Pane>, writer: &Mutex<Conn>, flow: &ConnFlow, mut next: u64) {
    loop {
        let step = {
            let mut st = pane.lock_state();
            loop {
                if flow.closed.load(Ordering::SeqCst) {
                    break PumpStep::Stop;
                }
                let start = st.ring.start_offset();
                let end = st.ring.end_offset();
                if !flow.paused.load(Ordering::SeqCst) {
                    if next < start {
                        break PumpStep::Lost {
                            from: next,
                            to: start,
                        };
                    }
                    if next < end {
                        let (from, bytes) = st.ring.slice_from(next, MAX_OUTPUT_CHUNK);
                        break PumpStep::Output { from, bytes };
                    }
                }
                // The exit is not output: it is sent once everything before it
                // is, paused or not.
                if let Some(exit) = st.exit {
                    if next >= end {
                        break PumpStep::Exit(exit);
                    }
                }
                st = pane.wait_changed(st, PUMP_POLL);
            }
        };
        let sent = match step {
            PumpStep::Stop => return,
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
                let sent = to_payload(&Reply::Exit(exit))
                    .map_err(io::Error::other)
                    .and_then(|p| send_frame(writer, KIND_CONTROL, &p, PUMP_WRITE_TIMEOUT))
                    .and_then(|()| {
                        writer
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .flush_to_peer()
                    });
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
        (_, Request::PrepareUpgrade) => Action::Reply(Reply::Unsupported {
            verb: "prepare_upgrade".into(),
            reason: "holder upgrade is plan Phase 8; this build does not implement it".into(),
        }),
        // Version 1 verbs.
        (Negotiated(v) | Attached(v) | InputOnly(v), Request::Ping) if v >= 1 => {
            Action::Reply(Reply::Pong)
        }
        // Version 2 verbs: the data path.
        (Negotiated(v), Request::Attach { from_offset }) if v >= DATA_PATH_VERSION => {
            Action::Attach(from_offset)
        }
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
        let timeout = Duration::from_millis(300);
        let params = ConnParams {
            handshake_timeout: timeout,
            input_queue_timeout: INPUT_QUEUE_TIMEOUT,
            expected_uid: transport::own_uid(),
        };

        let (mut client, t) = serve_pair(params);
        // Bounded: a server that never closes fails the test, not hangs it.
        client.set_read_timeout(Some(timeout * 5)).unwrap();
        let started = Instant::now();
        let mut buf = [0u8; 8];
        // The server closes: EOF, with nothing sent.
        assert_eq!(client.read(&mut buf).unwrap(), 0);
        t.join().unwrap();
        let took = started.elapsed();
        assert!(took >= timeout && took < timeout * 3, "idle: {took:?}");

        let (mut client, t) = serve_pair(params);
        let started = Instant::now();
        // A hello prefix, one byte every 100 ms: each byte lands inside a
        // per-read timeout, so only a whole-frame deadline can stop it.
        let hello = crate::frame::encode_frame(
            KIND_CONTROL,
            &to_payload(&Request::Hello { versions: vec![1] }).unwrap(),
        )
        .unwrap();
        for b in hello {
            if client.write_all(&[b]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            if started.elapsed() > timeout * 4 {
                break;
            }
        }
        t.join().unwrap();
        let took = started.elapsed();
        assert!(took < timeout * 3, "trickle held the thread: {took:?}");
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
                    from_offset: Some(5)
                },
                &i
            ),
            Action::Attach(Some(5))
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
            handle_request(&mut s, Request::Attach { from_offset: None }, &i),
            Action::Reject(RejectReason::AlreadyAttached, _)
        ));
        assert_eq!(handle_request(&mut s, Request::Detach, &i), Action::Detach);

        // A version-1 connection gets none of it.
        let mut s = ConnState::AwaitHello;
        handle_request(&mut s, Request::Hello { versions: vec![1] }, &i);
        assert_eq!(s, ConnState::Negotiated(1));
        for req in [
            Request::Attach { from_offset: None },
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
        for req in [Request::Attach { from_offset: None }, Request::OpenInput] {
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

    #[test]
    fn pty_holder_start_refuses_a_relative_pane_dir() {
        let err =
            Holder::start(Path::new("relative/panes"), &PaneId::new("p").unwrap()).unwrap_err();
        assert!(matches!(err, StartError::PaneDir(_)), "{err}");
    }
}
