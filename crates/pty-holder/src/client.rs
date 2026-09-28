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
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::frame::{read_frame, write_frame, Frame, KIND_CONTROL};
use crate::lock::{read_record, LockRecord, PaneLock, TryLock};
use crate::pane::{lock_path, PaneId};
use crate::protocol::{
    parse_reply, to_payload, CensusReply, HelloAck, Reply, Request, PROTOCOL_VERSIONS,
};
use crate::transport::{self, remaining, Conn};

/// Why [`connect`] did not produce a [`Client`].
#[derive(Debug)]
pub enum ConnectError {
    /// The holder answered, and speaks none of our versions. It is alive.
    Incompatible { holder_versions: Vec<u32> },
    /// The holder answered something that is not a handshake reply.
    Protocol(String),
    /// Connect, write, read or deadline failure.
    Io(io::Error),
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
#[derive(Debug)]
pub struct Client {
    conn: Conn,
    ack: HelloAck,
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
        Reply::HelloAck(ack) => Ok(Client { conn, ack }),
        Reply::NoCommonVersion { holder_versions } => {
            Err(ConnectError::Incompatible { holder_versions })
        }
        other => Err(ConnectError::Protocol(format!(
            "expected hello_ack, got {other:?}"
        ))),
    }
}

fn send(conn: &mut Conn, req: &Request, deadline: Instant) -> io::Result<()> {
    conn.set_timeout(Some(remaining(deadline)?))?;
    write_frame(conn, KIND_CONTROL, &to_payload(req))
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
    conn.set_timeout(Some(remaining(deadline)?))?;
    read_frame(conn)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "holder closed the connection"))
}

impl Client {
    /// The holder's handshake answer.
    pub fn hello_ack(&self) -> &HelloAck {
        &self.ack
    }

    /// Send one request and read one reply by `deadline`.
    pub fn request(&mut self, req: &Request, deadline: Instant) -> Result<Reply, ConnectError> {
        send(&mut self.conn, req, deadline)?;
        recv(&mut self.conn, deadline)
    }

    /// `ping` → `pong`.
    pub fn ping(&mut self, deadline: Instant) -> Result<(), ConnectError> {
        match self.request(&Request::Ping, deadline)? {
            Reply::Pong => Ok(()),
            other => Err(ConnectError::Protocol(format!(
                "expected pong, got {other:?}"
            ))),
        }
    }

    /// `census` → what the holder says about itself.
    pub fn census(&mut self, deadline: Instant) -> Result<CensusReply, ConnectError> {
        match self.request(&Request::Census, deadline)? {
            Reply::CensusReply(c) => Ok(c),
            other => Err(ConnectError::Protocol(format!(
                "expected census_reply, got {other:?}"
            ))),
        }
    }

    /// Send a raw frame. For tests of the holder's refusal paths; a runner
    /// sends typed requests.
    pub fn send_raw_frame(
        &mut self,
        kind: u8,
        payload: &[u8],
        deadline: Instant,
    ) -> io::Result<()> {
        self.conn.set_timeout(Some(remaining(deadline)?))?;
        write_frame(&mut self.conn, kind, payload)
    }

    /// Read one raw frame, or `None` at a clean EOF.
    pub fn recv_raw_frame(&mut self, deadline: Instant) -> io::Result<Option<Frame>> {
        self.conn.set_timeout(Some(remaining(deadline)?))?;
        read_frame(&mut self.conn)
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
        self.conn.set_timeout(Some(remaining(deadline)?))?;
        write_frame(&mut self.conn, kind, payload)
    }

    pub fn recv_frame(&mut self, deadline: Instant) -> io::Result<Option<Frame>> {
        self.conn.set_timeout(Some(remaining(deadline)?))?;
        read_frame(&mut self.conn)
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
/// An unreadable pane directory is an error — the census is UNKNOWN, not
/// empty. A missing one is an empty census (no holder was ever started there).
pub fn census(pane_dir: &Path, timeout: Duration) -> io::Result<Vec<CensusEntry>> {
    census_with_versions(pane_dir, timeout, PROTOCOL_VERSIONS)
}

/// [`census`] with an explicit version offer.
pub fn census_with_versions(
    pane_dir: &Path,
    timeout: Duration,
    versions: &[u32],
) -> io::Result<Vec<CensusEntry>> {
    let deadline = Instant::now() + timeout;
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
