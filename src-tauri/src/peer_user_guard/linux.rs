//! Linux owner resolver: the peer socket's uid.
//!
//! Primary: an exact `NETLINK_SOCK_DIAG` lookup (`inet_diag_req_v2` carrying
//! the full 4-tuple and no dump flag — the kernel's `inet_diag_find_one_icsk`),
//! which answers for exactly the one socket and returns its `idiag_uid`.
//! Fallback, when netlink is unavailable: a `/proc/net/tcp{,6}` scan matched on
//! the full 4-tuple, retried once on a miss — `/proc` is read in chunks that
//! can skip a row while sockets churn, which would otherwise turn into a
//! fail-closed refusal of a legitimate caller.
//!
//! A socket's uid is fixed when the socket is created, so there is no PID to
//! recycle between accept and lookup. Both sources read this process's network
//! namespace, the only one that can reach a loopback listener.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use super::decode::{
    canonical, owner_from_diag, owner_uid, parse_proc_line, parse_sock_diag_reply,
    sock_diag_request, ProcRow,
};
use super::{LocalUser, OwnerResolver, Resolution, Unresolved};

pub struct ProcNetResolver;

static SEQ: AtomicU32 = AtomicU32::new(1);

/// Netlink itself was unusable (socket refused, send/recv failed, garbage
/// reply): fall back to `/proc`. Distinct from the kernel ANSWERING "no such
/// socket", which is a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetlinkUnavailable;

/// One exact sock_diag lookup. `Err(NetlinkUnavailable)` means netlink itself is unusable
/// (socket refused, send/recv failed, garbage reply) and the caller should
/// fall back to `/proc`; `Ok(..)` is the kernel's answer.
pub fn netlink_owner(
    local: SocketAddr,
    peer: SocketAddr,
) -> Result<Result<u32, Unresolved>, NetlinkUnavailable> {
    let (local, peer) = (canonical(local), canonical(peer));
    if local.is_ipv4() != peer.is_ipv4() {
        return Ok(Err(Unresolved::NoMatchingSocket));
    }
    // SAFETY: plain socket(2); a negative return is checked.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_SOCK_DIAG,
        )
    };
    if fd < 0 {
        return Err(NetlinkUnavailable);
    }
    // SAFETY: `fd` is a fresh, owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let timeout = libc::timeval {
        tv_sec: 1,
        tv_usec: 0,
    };
    // SAFETY: valid fd and a correctly sized option value.
    unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let req = sock_diag_request(local, peer, seq);
    // SAFETY: zeroed sockaddr_nl with only the family set addresses the kernel.
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // SAFETY: `req` is a valid buffer; the address is a sockaddr_nl.
    let sent = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            req.as_ptr().cast(),
            req.len(),
            0,
            (&kernel as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if sent < 0 || sent as usize != req.len() {
        return Err(NetlinkUnavailable);
    }
    let mut buf = [0u8; 8192];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n <= 0 {
        return Err(NetlinkUnavailable);
    }
    match parse_sock_diag_reply(&buf[..n as usize]) {
        Some(reply) => Ok(owner_from_diag(reply)),
        None => Err(NetlinkUnavailable),
    }
}

fn rows(path: &str) -> std::io::Result<Vec<ProcRow>> {
    let file = std::fs::File::open(path)?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        if let Some(row) = parse_proc_line(&line?) {
            out.push(row);
        }
    }
    Ok(out)
}

/// The `/proc/net/tcp{,6}` scan. A peer connecting through an `AF_INET6`
/// socket to an IPv4 listener appears in tcp6 with a mapped address, so both
/// tables are read. A missing tcp6 (IPv6 disabled) is not an error.
pub fn proc_owner(local: SocketAddr, peer: SocketAddr) -> Result<u32, Unresolved> {
    let v4 = rows("/proc/net/tcp").map_err(|_| Unresolved::TableUnreadable)?;
    match owner_uid(v4, local, peer) {
        Err(Unresolved::NoMatchingSocket) => match rows("/proc/net/tcp6") {
            Ok(v6) => owner_uid(v6, local, peer),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Unresolved::NoMatchingSocket),
            Err(_) => Err(Unresolved::TableUnreadable),
        },
        other => other,
    }
}

impl OwnerResolver for ProcNetResolver {
    fn own_user(&self) -> Result<LocalUser, String> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        Ok(LocalUser::Uid(unsafe { libc::geteuid() }))
    }

    fn resolve(&self, local: SocketAddr, peer: SocketAddr, _accepted_at: SystemTime) -> Resolution {
        let owner = match netlink_owner(local, peer) {
            Ok(answer) => answer,
            Err(NetlinkUnavailable) => match proc_owner(local, peer) {
                // One retry: a chunked /proc read can miss a row mid-churn.
                Err(Unresolved::NoMatchingSocket) => proc_owner(local, peer),
                other => other,
            },
        };
        Resolution {
            owner: owner.map(LocalUser::Uid),
            pid: None,
        }
    }
}
