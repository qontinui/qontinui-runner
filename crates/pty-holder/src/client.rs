//! The runner side: connect + handshake, probe, census.
//!
//! **Health is an answered handshake within a deadline — never a successful
//! connect** (plan D3). A wedged holder's endpoint still accepts from the
//! kernel backlog, so a connect proves only that a socket exists.
//!
//! [`probe`] classifies one pane from two independent observables — the
//! handshake and the lock:
//!
//! | handshake            | lock        | verdict                        |
//! |----------------------|-------------|--------------------------------|
//! | `hello_ack`          | (not read)  | [`Probe::Healthy`]             |
//! | `no_common_version`  | (not read)  | [`Probe::Incompatible`]        |
//! | failed / timed out   | held        | [`Probe::Unknown`] — NEVER absent, NEVER healthy |
//! | failed / timed out   | acquirable  | [`Probe::Dead`]                |
//! | —                    | no file     | [`Probe::Absent`]              |
//!
//! [`census`] walks `*.lock` in a pane directory and probes every pane
//! concurrently under ONE overall deadline, on plain threads (no async
//! runtime). A pane whose probe has not reported by the deadline is counted as
//! `Unknown`, never dropped (D9: an unanswered holder is counted, as unknown).
//!
//! [`Client::attach`] (protocol version 2) turns a connection into the pane's
//! data path, split in two so one thread can read while another writes:
//! [`StreamReader::next_event`] yields output (with its absolute offset), loss
//! reports, replies and the final exit; [`StreamWriter`] sends input, resize,
//! pause/resume, kill and detach. Its requests are fire-and-forget on the
//! writer side; their `ok` replies arrive on the reader as [`Event::Reply`].
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::frame::{parse_output, read_frame, write_frame, Frame, KIND_CONTROL, KIND_DATA, KIND_OUTPUT};
use crate::lock::{read_record, LockRecord, PaneLock, TryLock};
use crate::pane::{check_private_dir, lock_path, PaneId};
use crate::protocol::{
    parse_reply, to_payload, AttachedReply, CensusReply, ExitReply, HelloAck, Reply, Request,
    DATA_PATH_VERSION, PROTOCOL_VERSIONS,
};
use crate::transport::{self, remaining, Conn, DeadlineIo};

/// Why [`connect`] did not produce a [`Client`].
#[derive(Debug)]
pub enum ConnectError {
    /// The holder answered, and speaks none of our versions. It is alive.
    Incompatible { holder_versions: Vec<u32> },
    /// The holder answered something that is not a handshake reply.
    Protocol(String),
    /// Connect, write, read or deadline failure.
    Io(io::Error),
    /// An earlier call on this [`Client`] failed, so the stream's position is
    /// unknown (a reply may be half-read, a request half-written). Nothing
    /// more is sent or read on it; the string is the failure that poisoned it.
    Poisoned(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Incompatible { holder_versions } => {
                write!(
                    f,
                    "no common protocol version; holder speaks {holder_versions:?}"
                )
            }
            ConnectError::Protocol(m) => write!(f, "protocol error: {m}"),
            ConnectError::Io(e) => write!(f, "{e}"),
            ConnectError::Poisoned(why) => write!(
                f,
                "connection poisoned by an earlier failure ({why}); reconnect"
            ),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        ConnectError::Io(e)
    }
}

/// A connection that completed the handshake.
///
/// **Poisoned on the first failure.** A deadline can expire in the middle of
/// a frame — after some of a request was written or some of a reply read —
/// and a stream in that state cannot be resynchronized: the next read would
/// start mid-frame or return the stale reply to the previous request, and the
/// holder would see a torn frame. So ANY I/O error, protocol error or
/// `rejected` reply (after which the holder closes) poisons the client, and
/// every later call returns [`ConnectError::Poisoned`] without touching the
/// stream. The remedy is a new [`connect`]. A flag rather than consuming
/// `self` on error keeps `ping`/`census` as plain `&mut self` calls and makes
/// "this client is dead" a typed, testable answer instead of a moved value.
///
/// **Drop clients promptly on EOF or error — before respawning the pane's
/// holder.** On Windows an open client handle keeps an instance of the pipe
/// name alive after its holder dies, and a new holder's
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` bind then fails with `ERROR_ACCESS_DENIED`
/// exactly as it would for a squatter.
#[derive(Debug)]
pub struct Client {
    conn: Conn,
    ack: HelloAck,
    poisoned: Option<String>,
}

/// Connect to a pane's holder and complete the handshake by `deadline`,
/// offering every version this build speaks.
pub fn connect(pane_dir: &Path, pane: &PaneId, deadline: Instant) -> Result<Client, ConnectError> {
    connect_with_versions(pane_dir, pane, deadline, PROTOCOL_VERSIONS)
}

/// [`connect`] offering an explicit version list (tests; and a runner that
/// wants to pin a version).
pub fn connect_with_versions(
    pane_dir: &Path,
    pane: &PaneId,
    deadline: Instant,
    versions: &[u32],
) -> Result<Client, ConnectError> {
    let mut conn = transport::connect(pane_dir, pane, deadline)?;
    #[cfg(unix)]
    {
        // The pane dir is 0700, so nobody else can bind there; checked anyway,
        // symmetric with the holder's own peer check.
        let uid = conn.peer_uid()?;
        if !transport::peer_authorized(uid, transport::own_uid()) {
            return Err(ConnectError::Protocol(format!(
                "endpoint is served by uid {uid}, not by this user"
            )));
        }
    }
    #[cfg(windows)]
    {
        // The pipe namespace is global: prove the server is the process that
        // holds this pane's lock, whose record sits in our own directory.
        let server = conn.server_pid()?;
        match read_record(&lock_path(pane_dir, pane)) {
            Some(r) if r.holder_pid == server => {}
            Some(r) => {
                return Err(ConnectError::Protocol(format!(
                    "pipe served by pid {server}, but the lock names holder pid {}",
                    r.holder_pid
                )))
            }
            None => {
                return Err(ConnectError::Protocol(format!(
                    "pipe served by pid {server}, and no lock record identifies the holder"
                )))
            }
        }
    }
    let hello = Request::Hello {
        versions: versions.to_vec(),
    };
    send(&mut conn, &hello, deadline)?;
    match recv(&mut conn, deadline)? {
        Reply::HelloAck(ack) => Ok(Client {
            conn,
            ack,
            poisoned: None,
        }),
        Reply::NoCommonVersion { holder_versions } => {
            Err(ConnectError::Incompatible { holder_versions })
        }
        other => Err(ConnectError::Protocol(format!(
            "expected hello_ack, got {other:?}"
        ))),
    }
}

fn send(conn: &mut Conn, req: &Request, deadline: Instant) -> io::Result<()> {
    let payload = to_payload(req).map_err(io::Error::other)?;
    write_frame(&mut DeadlineIo::new(conn, deadline), KIND_CONTROL, &payload)
}

fn recv(conn: &mut Conn, deadline: Instant) -> Result<Reply, ConnectError> {
    let frame = recv_frame(conn, deadline)?;
    if frame.kind != KIND_CONTROL {
        return Err(ConnectError::Protocol(format!(
            "expected a control frame, got kind 0x{:02x}",
            frame.kind
        )));
    }
    parse_reply(&frame.payload).map_err(|e| ConnectError::Protocol(e.to_string()))
}

fn recv_frame(conn: &mut Conn, deadline: Instant) -> io::Result<Frame> {
    read_frame(&mut DeadlineIo::new(conn, deadline))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "holder closed the connection"))
}

impl Client {
    /// The holder's handshake answer.
    pub fn hello_ack(&self) -> &HelloAck {
        &self.ack
    }

    /// Why this client refuses further calls, if it does.
    pub fn poisoned(&self) -> Option<&str> {
        self.poisoned.as_deref()
    }

    fn check(&self) -> Result<(), ConnectError> {
        match &self.poisoned {
            Some(why) => Err(ConnectError::Poisoned(why.clone())),
            None => Ok(()),
        }
    }

    /// Record `result`'s failure, if any, as the poison.
    fn poison_on_err<T>(&mut self, result: Result<T, ConnectError>) -> Result<T, ConnectError> {
        if let Err(e) = &result {
            self.poisoned = Some(e.to_string());
        }
        result
    }

    /// Send one request and read one reply by `deadline`.
    ///
    /// Poisons the client on any failure, and on a `rejected` reply (the
    /// holder closes the connection after one), which is still returned.
    pub fn request(&mut self, req: &Request, deadline: Instant) -> Result<Reply, ConnectError> {
        self.check()?;
        let result = send(&mut self.conn, req, deadline)
            .map_err(ConnectError::from)
            .and_then(|()| recv(&mut self.conn, deadline));
        let result = self.poison_on_err(result);
        if let Ok(Reply::Rejected { reason, detail }) = &result {
            self.poisoned = Some(format!(
                "holder rejected the request ({reason:?}: {detail})"
            ));
        }
        result
    }

    /// `ping` → `pong`.
    pub fn ping(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        match self.request(&Request::Ping, deadline)? {
            Reply::Pong => Ok(()),
            other => self.poison_on_err(Err(ConnectError::Protocol(format!(
                "expected pong, got {other:?}"
            )))),
        }
    }

    /// `census` → what the holder says about itself.
    pub fn census(&mut self, deadline: Instant) -> Result<CensusReply, ConnectError> {
        match self.request(&Request::Census, deadline)? {
            Reply::CensusReply(c) => Ok(c),
            other => self.poison_on_err(Err(ConnectError::Protocol(format!(
                "expected census_reply, got {other:?}"
            )))),
        }
    }

    /// Send a raw frame. For tests of the holder's refusal paths; a runner
    /// sends typed requests. Poisons on failure, like [`Client::request`].
    pub fn send_raw_frame(
        &mut self,
        kind: u8,
        payload: &[u8],
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        self.check()?;
        let r = write_frame(
            &mut DeadlineIo::new(&mut self.conn, deadline),
            kind,
            payload,
        )
        .map_err(ConnectError::from);
        self.poison_on_err(r)
    }

    /// Read one raw frame, or `None` at a clean EOF. Poisons on failure.
    pub fn recv_raw_frame(&mut self, deadline: Instant) -> Result<Option<Frame>, ConnectError> {
        self.check()?;
        let r =
            read_frame(&mut DeadlineIo::new(&mut self.conn, deadline)).map_err(ConnectError::from);
        self.poison_on_err(r)
    }
}

impl Client {
    /// Attach to the pane's output (protocol version 2): send `attach`, read
    /// `attached`, and split the connection into a reader and a writer.
    ///
    /// `from_offset: None` is a fresh consumer (the ring's last
    /// `ATTACH_TAIL_BYTES`); `Some(n)` resumes at the first byte the caller has
    /// not seen. Consumes the client: the connection now belongs to the stream.
    pub fn attach(
        mut self,
        from_offset: Option<u64>,
        deadline: Instant,
    ) -> Result<AttachedStream, ConnectError> {
        self.check()?;
        if self.ack.version < DATA_PATH_VERSION {
            return Err(ConnectError::Protocol(format!(
                "holder negotiated version {}; the data path needs {DATA_PATH_VERSION}",
                self.ack.version
            )));
        }
        let info = match self.request(&Request::Attach { from_offset }, deadline)? {
            Reply::Attached(a) => a,
            other => {
                return Err(ConnectError::Protocol(format!(
                    "expected attached, got {other:?}"
                )))
            }
        };
        let writer = self.conn.try_clone()?;
        Ok(AttachedStream {
            info,
            hello_ack: self.ack,
            reader: StreamReader { conn: self.conn },
            writer: StreamWriter { conn: writer },
        })
    }
}

/// An attached connection, split. Drop both halves to close it (the child
/// keeps running; [`StreamWriter::detach`] says so explicitly first).
#[derive(Debug)]
pub struct AttachedStream {
    /// Where the stream begins; see `protocol::AttachedReply`.
    pub info: AttachedReply,
    pub hello_ack: HelloAck,
    pub reader: StreamReader,
    pub writer: StreamWriter,
}

/// One thing the holder sent on an attached connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Pane output: `bytes` begin at absolute stream offset `offset`.
    Output { offset: u64, bytes: Vec<u8> },
    /// `[from_offset, to_offset)` left the holder's ring before it reached
    /// this connection. The stream continues at `to_offset`.
    Lost { from_offset: u64, to_offset: u64 },
    /// The child exited; the last frame of the stream.
    Exit(ExitReply),
    /// Any other reply — `ok {verb}` to a writer-side request, `pong`,
    /// `census_reply`, or a `rejected` (after which the holder closes).
    Reply(Reply),
}

/// The reading half of an attached connection.
#[derive(Debug)]
pub struct StreamReader {
    conn: Conn,
}

impl StreamReader {
    /// The next event, or `None` at a clean end of stream (the holder closed
    /// the connection — after an `exit`, or because it dropped a client that
    /// stopped reading). `deadline: None` waits indefinitely.
    ///
    /// Any error leaves the stream position unknown: drop the stream and
    /// reattach at the last offset seen.
    pub fn next_event(&mut self, deadline: Option<Instant>) -> Result<Option<Event>, ConnectError> {
        let frame = match deadline {
            Some(d) => read_frame(&mut DeadlineIo::new(&mut self.conn, d))?,
            None => {
                self.conn.set_read_timeout(None)?;
                read_frame(&mut self.conn)?
            }
        };
        let Some(frame) = frame else {
            return Ok(None);
        };
        match frame.kind {
            KIND_OUTPUT => {
                let (offset, bytes) = parse_output(&frame.payload).ok_or_else(|| {
                    ConnectError::Protocol("output frame shorter than its offset header".into())
                })?;
                Ok(Some(Event::Output {
                    offset,
                    bytes: bytes.to_vec(),
                }))
            }
            KIND_CONTROL => {
                let reply = parse_reply(&frame.payload)
                    .map_err(|e| ConnectError::Protocol(e.to_string()))?;
                Ok(Some(match reply {
                    Reply::OutputLost {
                        from_offset,
                        to_offset,
                    } => Event::Lost {
                        from_offset,
                        to_offset,
                    },
                    Reply::Exit(e) => Event::Exit(e),
                    other => Event::Reply(other),
                }))
            }
            other => Err(ConnectError::Protocol(format!(
                "unexpected frame kind 0x{other:02x} on an attached connection"
            ))),
        }
    }
}

/// The writing half of an attached connection. Every call is bounded by its
/// `deadline`; a failed call leaves the stream unusable (reattach).
#[derive(Debug)]
pub struct StreamWriter {
    conn: Conn,
}

impl StreamWriter {
    /// Raw input bytes for the child. No reply.
    pub fn input(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), ConnectError> {
        write_frame(&mut DeadlineIo::new(&mut self.conn, deadline), KIND_DATA, bytes)?;
        Ok(())
    }

    /// Send a request; its reply arrives on the [`StreamReader`].
    pub fn request(&mut self, req: &Request, deadline: Instant) -> Result<(), ConnectError> {
        send(&mut self.conn, req, deadline)?;
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16, deadline: Instant) -> Result<(), ConnectError> {
        self.request(&Request::Resize { cols, rows }, deadline)
    }

    pub fn pause(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        self.request(&Request::Pause, deadline)
    }

    pub fn resume(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        self.request(&Request::Resume, deadline)
    }

    pub fn kill(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        self.request(&Request::Kill, deadline)
    }

    /// Leave without ending the child. The holder answers `ok` and closes.
    pub fn detach(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        self.request(&Request::Detach, deadline)
    }

    /// A second handle on this connection that can end it while this writer
    /// is blocked mid-write (a full socket behind a child that does not read
    /// its stdin). Ending the connection is itself a detach: the holder keeps
    /// the child.
    pub fn shutdown_handle(&self) -> io::Result<ShutdownHandle> {
        Ok(ShutdownHandle {
            conn: self.conn.try_clone()?,
        })
    }
}

/// See [`StreamWriter::shutdown_handle`].
#[derive(Debug)]
pub struct ShutdownHandle {
    conn: Conn,
}

impl ShutdownHandle {
    /// End the connection in both directions, for every handle on it.
    pub fn shutdown(&self) {
        self.conn.shutdown();
    }
}

/// Open a connection WITHOUT the handshake, for tests that must speak first
/// out of turn (a non-`hello` first frame, a data frame, an unknown kind).
pub fn connect_raw(pane_dir: &Path, pane: &PaneId, deadline: Instant) -> io::Result<RawConn> {
    Ok(RawConn {
        conn: transport::connect(pane_dir, pane, deadline)?,
    })
}

/// An un-handshaken connection. See [`connect_raw`].
#[derive(Debug)]
pub struct RawConn {
    conn: Conn,
}

impl RawConn {
    pub fn send_frame(&mut self, kind: u8, payload: &[u8], deadline: Instant) -> io::Result<()> {
        write_frame(
            &mut DeadlineIo::new(&mut self.conn, deadline),
            kind,
            payload,
        )
    }

    pub fn recv_frame(&mut self, deadline: Instant) -> io::Result<Option<Frame>> {
        read_frame(&mut DeadlineIo::new(&mut self.conn, deadline))
    }
}

/// One pane's liveness verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The holder answered the handshake in time.
    Healthy { hello_ack: HelloAck },
    /// Something holds the lock, but no handshake was answered in time: a
    /// holder that is starting, wedged, or unreachable. Counted, never
    /// dropped, never rendered as healthy.
    Unknown {
        reason: String,
        record: Option<LockRecord>,
    },
    /// The lock is acquirable: whatever held it is dead.
    Dead { record: Option<LockRecord> },
    /// A live holder that speaks none of our versions (D15: listed, never
    /// killed, hidden or counted as zero).
    Incompatible { holder_versions: Vec<u32> },
    /// No lock file for this pane at all. Only a direct [`probe`] can see this;
    /// a [`census`] walks lock files and so never reports it.
    Absent,
}

/// Probe one pane, offering every version this build speaks.
pub fn probe(pane_dir: &Path, pane: &PaneId, deadline: Instant) -> Probe {
    probe_with_versions(pane_dir, pane, deadline, PROTOCOL_VERSIONS)
}

/// [`probe`] with an explicit version offer.
///
/// The handshake is tried FIRST and the lock only when it fails, so probing a
/// healthy holder never touches its lock. Taking the lock of a dead holder is
/// momentary (released before returning). A holder that is being spawned at
/// the same instant can lose its lock race to a probe and exit `lock held`, so
/// the runner must not probe a pane it is in the middle of spawning.
pub fn probe_with_versions(
    pane_dir: &Path,
    pane: &PaneId,
    deadline: Instant,
    versions: &[u32],
) -> Probe {
    // Never trust a directory we did not verify: a pane dir another user can
    // write to could hold a planted lock file or endpoint (Unix; a no-op on
    // Windows, where the pipe's owner is proven against the lock record).
    match check_private_dir(pane_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Probe::Absent,
        Err(e) => {
            return Probe::Unknown {
                reason: format!("pane dir refused: {e}"),
                record: None,
            }
        }
    }
    let lock_file = lock_path(pane_dir, pane);
    match std::fs::symlink_metadata(&lock_file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Probe::Absent,
        Err(e) => {
            return Probe::Unknown {
                reason: format!("lock file unreadable: {e}"),
                record: None,
            }
        }
        Ok(_) => {}
    }
    let handshake_err = match connect_with_versions(pane_dir, pane, deadline, versions) {
        Ok(client) => {
            return Probe::Healthy {
                hello_ack: client.ack,
            }
        }
        Err(ConnectError::Incompatible { holder_versions }) => {
            return Probe::Incompatible { holder_versions }
        }
        Err(e) => e,
    };
    let record = read_record(&lock_file);
    match PaneLock::try_acquire_existing(&lock_file) {
        Ok(TryLock::Acquired(lock)) => {
            drop(lock);
            Probe::Dead { record }
        }
        Ok(TryLock::Held) => Probe::Unknown {
            reason: format!("lock held but handshake unanswered: {handshake_err}"),
            record,
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Probe::Absent,
        Err(e) => Probe::Unknown {
            reason: format!("handshake failed ({handshake_err}) and lock unreadable ({e})"),
            record,
        },
    }
}

/// One census row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusEntry {
    /// The lock file's stem. Not necessarily a valid [`PaneId`] — a foreign
    /// file is reported, as Unknown, rather than silently skipped.
    pub pane_id: String,
    pub probe: Probe,
}

/// Probe every `*.lock` in `pane_dir` concurrently, all under ONE deadline
/// `timeout` from now. Returns rows sorted by pane id.
///
/// An unreadable pane directory — or, on Unix, one that is a symlink, not
/// ours, or group/other-writable — is an error: the census is UNKNOWN, not
/// empty. A missing one is an empty census (no holder was ever started there).
///
/// **The same lock race as [`probe`]:** probing a pane whose holder has not
/// yet answered takes that pane's lock for a moment when the handshake fails,
/// and a holder being spawned at that instant loses its lock race and exits
/// `lock held`. Pass the panes the caller is currently spawning in `skip`:
/// they are not probed and are reported as `Unknown` ("being spawned by the
/// caller") — counted, never dropped.
pub fn census(pane_dir: &Path, timeout: Duration, skip: &[PaneId]) -> io::Result<Vec<CensusEntry>> {
    census_with_versions(pane_dir, timeout, skip, PROTOCOL_VERSIONS)
}

/// [`census`] with an explicit version offer.
pub fn census_with_versions(
    pane_dir: &Path,
    timeout: Duration,
    skip: &[PaneId],
    versions: &[u32],
) -> io::Result<Vec<CensusEntry>> {
    let deadline = Instant::now() + timeout;
    match check_private_dir(pane_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let entries = match std::fs::read_dir(pane_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut stems: Vec<String> = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("lock") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            // Not UTF-8, so certainly not a pane id; report it by its display form.
            None => format!("{}", path.display()),
        };
        stems.push(stem);
    }

    let (tx, rx) = mpsc::channel::<CensusEntry>();
    let mut rows: Vec<CensusEntry> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for stem in stems {
        let pane = match PaneId::new(&stem) {
            Ok(p) => p,
            Err(e) => {
                rows.push(CensusEntry {
                    pane_id: stem,
                    probe: Probe::Unknown {
                        reason: e.to_string(),
                        record: None,
                    },
                });
                continue;
            }
        };
        if skip.contains(&pane) {
            rows.push(CensusEntry {
                probe: Probe::Unknown {
                    reason: "not probed: being spawned by the caller".into(),
                    record: read_record(&lock_path(pane_dir, &pane)),
                },
                pane_id: stem,
            });
            continue;
        }
        pending.push(stem.clone());
        let tx = tx.clone();
        let dir = pane_dir.to_path_buf();
        let versions = versions.to_vec();
        let spawned = std::thread::Builder::new()
            .name("pty-holder-census".into())
            .spawn(move || {
                let probe = probe_with_versions(&dir, &pane, deadline, &versions);
                let _ = tx.send(CensusEntry {
                    pane_id: stem,
                    probe,
                });
            });
        // A thread that could not be spawned never reports; its pane stays in
        // `pending` and is counted as Unknown below.
        drop(spawned);
    }
    drop(tx);

    while !pending.is_empty() {
        let left = match remaining(deadline) {
            Ok(d) => d,
            Err(_) => break,
        };
        match rx.recv_timeout(left) {
            Ok(row) => {
                pending.retain(|p| p != &row.pane_id);
                rows.push(row);
            }
            // Timeout, or every sender gone (a probe thread that never
            // started or panicked): stop waiting; the rest are Unknown.
            Err(_) => break,
        }
    }
    for pane_id in pending {
        rows.push(CensusEntry {
            probe: Probe::Unknown {
                reason: "census deadline elapsed before this pane's probe reported".into(),
                record: read_record(&pane_dir.join(format!("{pane_id}.lock"))),
            },
            pane_id,
        });
    }
    rows.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    Ok(rows)
}
