//! The holder side: lock, endpoint, and the per-connection verb dispatch.
//!
//! Start-up order is the whole of plan D3, per pane (D13):
//!
//! 1. Make sure the pane directory exists and (Unix) is ours and 0700.
//! 2. Take `<pane-id>.lock`, non-blocking. Held → another holder is alive for
//!    this pane → exit, touching nothing.
//! 3. Write the [`LockRecord`] (holder pid, versions, start time) into it.
//! 4. Unix: a `<pane-id>.sock` left by a dead holder is unlinked — only now,
//!    under the lock, when no live holder can own it.
//! 5. Bind the endpoint.
//!
//! Phase 1 has no PTY: the holder answers the handshake, `census`, `ping` and a
//! typed `unsupported` for `prepare_upgrade`, and serves until killed.
//!
//! The dispatch is a POSITIVE ALLOWLIST (plan D5): every frame kind, verb and
//! state combination not named in [`handle_request`]'s match falls through to
//! a typed `rejected` and the connection is closed. There is no default action.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::frame::{read_frame, write_frame, KIND_CONTROL, KIND_DATA};
use crate::lock::{LockRecord, PaneLock, TryLock};
use crate::pane::{lock_path, PaneId};
use crate::protocol::{
    holder_build, negotiate, parse_request, to_payload, CensusReply, HelloAck, RejectReason, Reply,
    Request, PROTOCOL_VERSIONS,
};
use crate::transport::{self, Conn, DeadlineIo, Listener};

/// Most concurrent connections one holder serves. Each costs a thread; the
/// runner needs one or two. Past this a client gets `rejected {busy}`.
pub const MAX_CONNECTIONS: usize = 16;

/// How long a fresh connection has to send its `hello`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on writing one reply.
pub const REPLY_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// First pause after a failed accept.
pub const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(10);
/// Longest pause between accept retries.
pub const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

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

/// A holder that holds its lock and has bound its endpoint.
#[derive(Debug)]
pub struct Holder {
    info: Arc<HolderInfo>,
    listener: Listener,
    // Held for the holder's lifetime; dropping it releases the pane.
    _lock: PaneLock,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Create the pane dir if needed, and refuse one this user cannot trust.
///
/// Unix: a directory we CREATE is made 0700 (the umask can only narrow it). A
/// PRE-EXISTING one must be a real directory (not a symlink), ours, and not
/// group/other-writable — anyone who could write there could already have
/// planted a lock file or socket, so it is refused rather than chmod-repaired.
/// A pre-existing directory that is merely group/other-READABLE is tightened
/// to 0700: nothing could have been planted, and the endpoint's path should
/// not be listable.
fn prepare_pane_dir(pane_dir: &Path) -> Result<(), StartError> {
    if !pane_dir.is_absolute() {
        return Err(StartError::PaneDir(format!(
            "{} is not absolute; the runner resolves it and passes it whole",
            pane_dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(pane_dir)?;
        match crate::pane::check_private_dir(pane_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(StartError::PaneDir(e.to_string()))
            }
            Err(e) => return Err(e.into()),
        }
        let md = std::fs::symlink_metadata(pane_dir)?;
        if md.mode() & 0o077 != 0 {
            std::fs::set_permissions(pane_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(windows)]
    {
        // The directory inherits the per-user app-data ACL it is created under;
        // the Windows access barrier is the pipe's DACL (transport::windows).
        std::fs::create_dir_all(pane_dir)?;
        crate::pane::check_private_dir(pane_dir).map_err(|e| StartError::PaneDir(e.to_string()))?;
    }
    Ok(())
}

impl Holder {
    /// Steps 1–5 of the module docs. Nothing is bound unless the lock is ours.
    pub fn start(pane_dir: &Path, pane_id: &PaneId) -> Result<Holder, StartError> {
        prepare_pane_dir(pane_dir)?;
        let lock_file = lock_path(pane_dir, pane_id);
        let mut lock = match PaneLock::try_acquire(&lock_file)? {
            TryLock::Acquired(l) => l,
            TryLock::Held => {
                return Err(StartError::LockHeld {
                    record: crate::lock::read_record(&lock_file),
                })
            }
        };
        let info = HolderInfo {
            pane_id: pane_id.clone(),
            holder_pid: std::process::id(),
            child_pid: None,
            started_at_unix_ms: now_unix_ms(),
        };
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
                Err(e) => return Err(e.into()),
            }
        }
        let listener = transport::bind(pane_dir, pane_id)?;
        Ok(Holder {
            info: Arc::new(info),
            listener,
            _lock: lock,
        })
    }

    pub fn info(&self) -> &HolderInfo {
        &self.info
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

    /// Accept forever, one thread per connection, at most
    /// [`MAX_CONNECTIONS`] at once.
    ///
    /// An accept error never ends the holder — the pane it holds outlives any
    /// transient endpoint trouble — it backs off exponentially from
    /// [`ACCEPT_BACKOFF_MIN`] to [`ACCEPT_BACKOFF_MAX`] and retries. The one
    /// exception is [`transport::accept_error_is_fatal`]: on Windows, a pipe
    /// name found squatted when the listener had to re-create it from zero
    /// instances. Then there is no endpoint this holder can safely serve, and
    /// `serve` returns that error.
    pub fn serve(self) -> io::Result<()> {
        let active = Arc::new(AtomicUsize::new(0));
        let mut backoff = ACCEPT_BACKOFF_MIN;
        loop {
            let conn = match self.listener.accept() {
                Ok(c) => {
                    backoff = ACCEPT_BACKOFF_MIN;
                    c
                }
                Err(e) if transport::accept_error_is_fatal(&e) => return Err(e),
                Err(_) => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    continue;
                }
            };
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                active.fetch_sub(1, Ordering::SeqCst);
                let mut conn = conn;
                reject(
                    &mut conn,
                    RejectReason::Busy,
                    "holder is at its connection cap",
                );
                continue;
            }
            let info = Arc::clone(&self.info);
            let slot = Arc::clone(&active);
            let spawned = std::thread::Builder::new()
                .name("pty-holder-conn".into())
                .spawn(move || {
                    serve_conn(conn, &info);
                    slot.fetch_sub(1, Ordering::SeqCst);
                });
            if spawned.is_err() {
                // The closure (and its conn) was dropped, so the client sees
                // EOF; give the slot back.
                active.fetch_sub(1, Ordering::SeqCst);
            }
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
}

/// What the dispatcher decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Send this reply and keep serving.
    Reply(Reply),
    /// Send `rejected` and close.
    Reject(RejectReason, String),
}

/// Authorize the peer, then dispatch frames until EOF, error or rejection.
pub fn serve_conn(conn: Conn, info: &HolderInfo) {
    serve_conn_with(
        conn,
        info,
        ConnParams {
            handshake_timeout: HANDSHAKE_TIMEOUT,
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
    /// The uid a peer must have.
    #[cfg(unix)]
    pub expected_uid: u32,
}

pub(crate) fn serve_conn_with(mut conn: Conn, info: &HolderInfo, params: ConnParams) {
    #[cfg(unix)]
    {
        match conn.peer_uid() {
            Ok(uid) if transport::peer_authorized(uid, params.expected_uid) => {}
            Ok(uid) => {
                reject(
                    &mut conn,
                    RejectReason::PeerNotAuthorized,
                    &format!("peer uid {uid} is not the holder's user"),
                );
                return;
            }
            Err(e) => {
                reject(
                    &mut conn,
                    RejectReason::PeerNotAuthorized,
                    &format!("peer credentials unavailable: {e}"),
                );
                return;
            }
        }
    }
    // Windows: the pipe's DACL already refused every other user at open time.

    let handshake_deadline = Instant::now() + params.handshake_timeout;
    let mut state = ConnState::AwaitHello;
    loop {
        let read = if state == ConnState::AwaitHello {
            // One deadline for the whole first frame (DeadlineIo re-arms it
            // before every read), so a trickling client cannot hold a thread.
            read_frame(&mut DeadlineIo::new(&mut conn, handshake_deadline))
        } else {
            // Handshake done: an idle runner connection is legitimate.
            let _ = conn.set_timeout(None);
            read_frame(&mut conn)
        };
        let frame = match read {
            Ok(Some(f)) => f,
            // EOF, timeout, truncated or oversized frame: nothing more to say.
            Ok(None) | Err(_) => return,
        };
        let action = match frame.kind {
            KIND_CONTROL => match parse_request(&frame.payload) {
                Ok(req) => handle_request(&mut state, req, info),
                Err((reason, detail)) => Action::Reject(reason, detail),
            },
            KIND_DATA => Action::Reject(
                RejectReason::UnexpectedDataFrame,
                "this holder accepts no data frames (no PTY in this build)".into(),
            ),
            other => Action::Reject(
                RejectReason::UnknownFrameKind,
                format!("frame kind 0x{other:02x} is not accepted"),
            ),
        };
        match action {
            Action::Reply(reply) => {
                // A client that stops reading cannot pin this thread on a
                // full socket buffer.
                let write_deadline = Instant::now() + REPLY_WRITE_TIMEOUT;
                let mut io = DeadlineIo::new(&mut conn, write_deadline);
                if write_frame(&mut io, KIND_CONTROL, &to_payload(&reply)).is_err() {
                    return;
                }
            }
            Action::Reject(reason, detail) => {
                reject(&mut conn, reason, &detail);
                return;
            }
        }
    }
}

/// The allowlist. Every arm is a verb this build implements in that state;
/// the final arm is the refusal.
fn handle_request(state: &mut ConnState, req: Request, info: &HolderInfo) -> Action {
    match (*state, req) {
        (ConnState::AwaitHello, Request::Hello { versions }) => {
            match negotiate(&versions, PROTOCOL_VERSIONS) {
                Some(version) => {
                    *state = ConnState::Negotiated(version);
                    Action::Reply(Reply::HelloAck(HelloAck {
                        version,
                        holder_build: holder_build(),
                        holder_pid: info.holder_pid,
                        child_pid: info.child_pid,
                    }))
                }
                None => {
                    *state = ConnState::EnvelopeOnly;
                    Action::Reply(Reply::NoCommonVersion {
                        holder_versions: PROTOCOL_VERSIONS.to_vec(),
                    })
                }
            }
        }
        (ConnState::AwaitHello, other) => Action::Reject(
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
        (ConnState::Negotiated(v), Request::Ping) if v >= 1 => Action::Reply(Reply::Pong),
        (_, other) => Action::Reject(
            RejectReason::VerbNotInVersion,
            format!(
                "{:?} is not available without a negotiated version",
                other.verb()
            ),
        ),
    }
}

/// Send `rejected` (best effort, bounded) and close.
fn reject(conn: &mut Conn, reason: RejectReason, detail: &str) {
    let reply = Reply::Rejected {
        reason,
        detail: detail.to_string(),
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let _ = write_frame(
        &mut DeadlineIo::new(conn, deadline),
        KIND_CONTROL,
        &to_payload(&reply),
    );
    conn.shutdown();
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
        let t =
            std::thread::spawn(move || serve_conn_with(Conn::from_stream(server), &info(), params));
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
            expected_uid: transport::own_uid(),
        };

        let (mut client, t) = serve_pair(params);
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
            &to_payload(&Request::Hello { versions: vec![1] }),
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
            expected_uid: transport::own_uid().wrapping_add(1),
        };
        let (mut client, t) = serve_pair(params);
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

    #[test]
    fn pty_holder_start_refuses_a_relative_pane_dir() {
        let err =
            Holder::start(Path::new("relative/panes"), &PaneId::new("p").unwrap()).unwrap_err();
        assert!(matches!(err, StartError::PaneDir(_)), "{err}");
    }
}
