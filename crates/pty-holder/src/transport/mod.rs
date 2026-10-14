//! The local IPC transport: one endpoint per pane, authorized by the OS.
//!
//! - Unix: a Unix-domain stream socket at `<pane-dir>/<pane-id>.sock`, mode
//!   0600, inside a pane directory the holder keeps at 0700; every accepted
//!   peer's uid is read from the kernel (`SO_PEERCRED` on Linux, `getpeereid`
//!   on macOS/BSD) and must equal the holder's (plan D5).
//! - Windows: a named pipe ([`crate::pane::pipe_name`]) whose DACL grants only
//!   the current user's SID, created with `PIPE_REJECT_REMOTE_CLIENTS` and
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (a pre-existing pipe of that name — a
//!   squatter — makes the bind fail instead of sharing it).
//!
//! This is not another unauthenticated local door (coord finding `df5eccc8`):
//! nothing on it is reachable without being the same OS user, and even then
//! only the allowlisted verbs are (`server`).
//!
//! Both [`Conn`]s are plain blocking `Read + Write` streams with an optional
//! timeout; no async runtime. A timed-out read or write surfaces as
//! `ErrorKind::TimedOut` on every platform.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::pane::PaneId;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{own_uid, Conn, Listener};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Conn, Listener};

/// Bind the pane's endpoint. The caller MUST already hold the pane lock
/// (lock-before-endpoint, plan D3) and must have removed any stale Unix socket
/// file under that lock.
pub fn bind(pane_dir: &Path, pane: &PaneId) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        Listener::bind(&crate::pane::socket_path(pane_dir, pane))
    }
    #[cfg(windows)]
    {
        Listener::bind(&crate::pane::pipe_name(pane_dir, pane))
    }
}

/// Connect to the pane's endpoint, giving up at `deadline`.
///
/// A successful connect says NOTHING about health — a wedged holder's socket
/// still accepts from the kernel backlog. Health is `client::connect`'s
/// answered handshake.
pub fn connect(pane_dir: &Path, pane: &PaneId, deadline: Instant) -> io::Result<Conn> {
    #[cfg(unix)]
    {
        unix::connect(&crate::pane::socket_path(pane_dir, pane), deadline)
    }
    #[cfg(windows)]
    {
        windows::connect(&crate::pane::pipe_name(pane_dir, pane), deadline)
    }
}

/// `Read + Write` over a [`Conn`] where EVERY syscall is bounded by what is
/// left of ONE deadline.
///
/// A timeout armed once per frame is a per-SYSCALL bound: `read_exact` loops,
/// and each of its reads would get the full remaining time again, so a peer
/// that trickles one byte just inside each timeout stretches a single frame to
/// N x the deadline. Re-arming `remaining(deadline)` before every read and
/// write makes the deadline a bound on the whole operation, on both OSes.
#[derive(Debug)]
pub struct DeadlineIo<'a> {
    conn: &'a mut Conn,
    deadline: Instant,
}

impl<'a> DeadlineIo<'a> {
    pub fn new(conn: &'a mut Conn, deadline: Instant) -> Self {
        DeadlineIo { conn, deadline }
    }
}

// Each direction arms only ITS OWN timeout: a connection split with
// `Conn::try_clone` has one thread reading and another writing the same socket,
// and on Unix the timeouts are per-socket, so arming both from either half
// would re-arm the other half's (see `Conn::set_read_timeout`).
impl io::Read for DeadlineIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.conn
            .set_read_timeout(Some(remaining(self.deadline)?))?;
        self.conn.read(buf)
    }
}

impl io::Write for DeadlineIo<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn
            .set_write_timeout(Some(remaining(self.deadline)?))?;
        self.conn.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush()
    }
}

/// The payload of an accept error that must END the holder rather than be
/// retried. Only the Windows listener produces one: a pipe name found squatted
/// when it had to be re-created from zero instances.
#[derive(Debug)]
pub struct FatalAcceptError(pub String);

impl std::fmt::Display for FatalAcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FatalAcceptError {}

/// Whether `Listener::accept`'s error is a [`FatalAcceptError`]. Every other
/// accept error is transient and retried with backoff.
pub fn accept_error_is_fatal(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<FatalAcceptError>())
}

/// Time left until `deadline`, or `TimedOut` when none is.
pub fn remaining(deadline: Instant) -> io::Result<Duration> {
    let now = Instant::now();
    if now >= deadline {
        Err(io::Error::new(io::ErrorKind::TimedOut, "deadline elapsed"))
    } else {
        Ok(deadline - now)
    }
}

/// Whether a peer may talk to this holder: same uid as the holder, nothing
/// else. Pure so the decision is tested without a second OS user.
pub fn peer_authorized(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// A connection's PERMANENT "shut down" mark, shared by every handle on it.
///
/// Unix gets this from the kernel: after `shutdown(SHUT_RDWR)` every later
/// read on the socket returns EOF and every write fails, whichever duplicate
/// it comes from. A Windows pipe has no such state — `CancelIoEx` cancels
/// only the I/O pending at that instant, and a `ReadFile`/`WriteFile` issued a
/// moment later proceeds as if nothing happened. So the Windows `Conn` carries
/// one of these, cloned into every `try_clone`: `shutdown` sets it and then
/// cancels, and every operation checks it before it starts and again after it
/// is issued (closing the check-then-issue race). Platform-independent so the
/// contract is unit-tested on every OS.
#[derive(Debug, Clone, Default)]
pub struct ShutFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ShutFlag {
    /// Mark the connection shut, for every handle sharing this flag. Call it
    /// BEFORE cancelling pending I/O, so an operation issued after the cancel
    /// sees it on its post-issue check.
    pub fn shut(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_shut(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// What an operation on a shut connection returns — the Unix
    /// `SHUT_RDWR` answer: EOF for a read, `BrokenPipe` for a write.
    pub fn result(is_read: bool) -> io::Result<usize> {
        if is_read {
            Ok(0)
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "connection was shut down",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review round 4: shutting a connection is PERMANENT and shared — a
    /// clone made before or after sees it, reads then read EOF and writes
    /// fail, exactly as a Unix socket after `SHUT_RDWR`.
    #[test]
    fn pty_holder_shut_flag_is_shared_and_permanent() {
        let a = ShutFlag::default();
        let before = a.clone();
        assert!(!a.is_shut() && !before.is_shut());
        a.shut();
        let after = a.clone();
        assert!(before.is_shut() && after.is_shut());
        assert_eq!(
            ShutFlag::result(true).unwrap(),
            0,
            "a read on a shut connection is EOF"
        );
        assert_eq!(
            ShutFlag::result(false).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        a.shut();
        assert!(before.is_shut(), "idempotent, never cleared");
    }

    #[test]
    fn pty_holder_peer_authorization_is_same_uid_only() {
        assert!(peer_authorized(1000, 1000));
        assert!(!peer_authorized(0, 1000), "root is not the holder's user");
        assert!(!peer_authorized(1001, 1000));
    }

    #[test]
    fn pty_holder_only_the_fatal_marker_is_fatal() {
        let fatal = io::Error::other(FatalAcceptError("squatted".into()));
        assert!(accept_error_is_fatal(&fatal));
        assert!(!accept_error_is_fatal(&io::Error::other("transient")));
        assert!(!accept_error_is_fatal(&io::Error::from(
            io::ErrorKind::Interrupted
        )));
    }

    #[test]
    fn pty_holder_remaining_is_timed_out_after_deadline() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(remaining(past).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(remaining(Instant::now() + Duration::from_secs(5)).is_ok());
    }
}
