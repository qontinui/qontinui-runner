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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_peer_authorization_is_same_uid_only() {
        assert!(peer_authorized(1000, 1000));
        assert!(!peer_authorized(0, 1000), "root is not the holder's user");
        assert!(!peer_authorized(1001, 1000));
    }

    #[test]
    fn pty_holder_remaining_is_timed_out_after_deadline() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(remaining(past).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(remaining(Instant::now() + Duration::from_secs(5)).is_ok());
    }
}
