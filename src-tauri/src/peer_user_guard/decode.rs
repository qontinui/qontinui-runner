//! Pure decoders for the two socket tables the guard reads.
//!
//! Platform-independent on purpose: the Windows row decoding is exercised by
//! the unit tests on every CI leg, including the Linux ones, because the
//! fleet has no Windows CI runner for qontinui-runner yet.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use super::Unresolved;

/// One row of `/proc/net/tcp` or `/proc/net/tcp6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcRow {
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub state: u8,
    pub uid: u32,
    pub inode: u64,
}

/// `TCP_TIME_WAIT` in the kernel's `tcp_states.h`.
pub const PROC_STATE_TIME_WAIT: u8 = 0x06;

/// Parse one data line of `/proc/net/tcp{,6}`. Returns `None` for the header
/// line and for anything malformed.
///
/// Field layout: `sl local rem st tx:rx tr:when retrnsmt uid timeout inode …`.
pub fn parse_proc_line(line: &str) -> Option<ProcRow> {
    let mut f = line.split_whitespace();
    let _sl = f.next()?;
    let local = parse_proc_addr(f.next()?)?;
    let remote = parse_proc_addr(f.next()?)?;
    let state = u8::from_str_radix(f.next()?, 16).ok()?;
    let _queues = f.next()?;
    let _timer = f.next()?;
    let _retrnsmt = f.next()?;
    let uid = f.next()?.parse::<u32>().ok()?;
    let _timeout = f.next()?;
    let inode = f.next()?.parse::<u64>().ok()?;
    Some(ProcRow {
        local,
        remote,
        state,
        uid,
        inode,
    })
}

/// `0100007F:2694` (IPv4) or a 32-hex-digit IPv6 address and a port. The
/// kernel prints each 32-bit word of the network-order address with `%08X`
/// as a NATIVE integer, so each word's bytes come back with `to_ne_bytes`.
fn parse_proc_addr(field: &str) -> Option<SocketAddr> {
    let (addr, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match addr.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(addr, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                let word = u32::from_str_radix(addr.get(i * 8..i * 8 + 8)?, 16).ok()?;
                chunk.copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Collapse an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to IPv4, so a
/// client that connected through an `AF_INET6` socket matches the IPv4
/// address the listener reports for it.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), addr.port()),
            None => addr,
        },
        IpAddr::V4(_) => addr,
    }
}

/// Find the uid owning the PEER end of `peer → local` among `rows`: the row
/// whose local address is the peer's and whose remote address is ours.
pub fn owner_uid<I: IntoIterator<Item = ProcRow>>(
    rows: I,
    local: SocketAddr,
    peer: SocketAddr,
) -> Result<u32, Unresolved> {
    let (local, peer) = (canonical(local), canonical(peer));
    for row in rows {
        if canonical(row.local) == peer && canonical(row.remote) == local {
            // A TIME_WAIT or orphaned socket reports uid 0 and inode 0: its
            // owner field is not the process that connected.
            if row.state == PROC_STATE_TIME_WAIT || row.inode == 0 {
                return Err(Unresolved::SocketClosing);
            }
            return Ok(row.uid);
        }
    }
    Err(Unresolved::NoMatchingSocket)
}

/// A Windows `MIB_TCP*ROW_OWNER_PID` port field: the port in network byte
/// order in the low 16 bits of a DWORD read natively (little-endian).
pub fn win_port(dw: u32) -> u16 {
    u16::from_be((dw & 0xFFFF) as u16)
}

/// A Windows `MIB_TCPROW_OWNER_PID` IPv4 address field: network-order bytes
/// read as a native DWORD.
pub fn win_ipv4(dw: u32) -> Ipv4Addr {
    Ipv4Addr::from(dw.to_ne_bytes())
}

/// A decoded Windows TCP table row, either family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinRow {
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub pid: u32,
}

/// The PID owning the peer end of `peer → local` among `rows`.
pub fn owner_pid<I: IntoIterator<Item = WinRow>>(
    rows: I,
    local: SocketAddr,
    peer: SocketAddr,
) -> Result<u32, Unresolved> {
    let (local, peer) = (canonical(local), canonical(peer));
    rows.into_iter()
        .find(|r| canonical(r.local) == peer && canonical(r.remote) == local)
        .map(|r| r.pid)
        .ok_or(Unresolved::NoMatchingSocket)
}

/// 100-ns intervals between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// A Windows `FILETIME` (two DWORDs of 100-ns ticks since 1601) as a
/// `SystemTime`; `None` before the Unix epoch.
pub fn filetime_to_system_time(low: u32, high: u32) -> Option<std::time::SystemTime> {
    let ticks = (u64::from(high) << 32) | u64::from(low);
    let since_unix = ticks.checked_sub(FILETIME_UNIX_EPOCH)?;
    Some(
        std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(since_unix / 10_000_000)
            + std::time::Duration::from_nanos((since_unix % 10_000_000) * 100),
    )
}

// ---------------------------------------------------------------------------
// NETLINK_SOCK_DIAG — the exact single-socket lookup (Linux)
// ---------------------------------------------------------------------------
//
// Layouts from the kernel uapi (`linux/netlink.h`, `linux/inet_diag.h`,
// `linux/sock_diag.h`). Encoded and decoded here, platform-independently, so
// the codec is unit-tested on every CI leg; only the socket I/O is Linux-only.

/// `SOCK_DIAG_BY_FAMILY`.
pub const SOCK_DIAG_BY_FAMILY: u16 = 20;
/// `NLMSG_ERROR`.
pub const NLMSG_ERROR: u16 = 2;
/// `NLM_F_REQUEST` — and deliberately NOT `NLM_F_DUMP`: without the dump flag
/// the kernel performs `inet_diag_find_one_icsk`, an exact 4-tuple lookup.
pub const NLM_F_REQUEST: u16 = 1;
/// Linux `AF_INET` / `AF_INET6` (the netlink family field, not Windows').
pub const LINUX_AF_INET: u8 = 2;
pub const LINUX_AF_INET6: u8 = 10;
const IPPROTO_TCP: u8 = 6;
const NLMSG_HDR_LEN: usize = 16;
/// `inet_diag_req_v2` = 8 bytes of header fields + a 48-byte `inet_diag_sockid`.
const INET_DIAG_REQ_V2_LEN: usize = 56;
/// Offsets of `idiag_state`, `idiag_uid` and `idiag_inode` in `inet_diag_msg`.
const MSG_STATE_OFF: usize = 1;
const MSG_UID_OFF: usize = 64;
const MSG_INODE_OFF: usize = 68;
/// `TCP_TIME_WAIT`, as for the `/proc` arm.
const DIAG_STATE_TIME_WAIT: u8 = PROC_STATE_TIME_WAIT;

fn addr_bytes(ip: IpAddr) -> [u8; 16] {
    let mut out = [0u8; 16];
    match ip {
        IpAddr::V4(v4) => out[..4].copy_from_slice(&v4.octets()),
        IpAddr::V6(v6) => out.copy_from_slice(&v6.octets()),
    }
    out
}

/// The exact-lookup request for the socket whose local end is `peer` and whose
/// remote end is `local` — i.e. the CLIENT socket of `peer → local`. Both
/// addresses must be the same family (canonicalise first).
pub fn sock_diag_request(local: SocketAddr, peer: SocketAddr, seq: u32) -> Vec<u8> {
    let family = if peer.is_ipv4() {
        LINUX_AF_INET
    } else {
        LINUX_AF_INET6
    };
    let total = NLMSG_HDR_LEN + INET_DIAG_REQ_V2_LEN;
    let mut b = Vec::with_capacity(total);
    // nlmsghdr
    b.extend_from_slice(&(total as u32).to_ne_bytes());
    b.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    b.extend_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    b.extend_from_slice(&seq.to_ne_bytes());
    b.extend_from_slice(&0u32.to_ne_bytes()); // nlmsg_pid: kernel
                                              // inet_diag_req_v2
    b.push(family);
    b.push(IPPROTO_TCP);
    b.push(0); // idiag_ext
    b.push(0); // pad
    b.extend_from_slice(&u32::MAX.to_ne_bytes()); // idiag_states: all
                                                  // inet_diag_sockid: ports and addresses are network order.
    b.extend_from_slice(&peer.port().to_be_bytes()); // idiag_sport
    b.extend_from_slice(&local.port().to_be_bytes()); // idiag_dport
    b.extend_from_slice(&addr_bytes(peer.ip())); // idiag_src
    b.extend_from_slice(&addr_bytes(local.ip())); // idiag_dst
    b.extend_from_slice(&0u32.to_ne_bytes()); // idiag_if
    b.extend_from_slice(&u32::MAX.to_ne_bytes()); // cookie[0]: INET_DIAG_NOCOOKIE
    b.extend_from_slice(&u32::MAX.to_ne_bytes()); // cookie[1]
    debug_assert_eq!(b.len(), total);
    b
}

/// What the kernel answered to one exact lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagReply {
    /// The socket exists: its state, owner uid and inode.
    Found { state: u8, uid: u32, inode: u32 },
    /// `NLMSG_ERROR` with `-ENOENT`: no such socket.
    NotFound,
    /// `NLMSG_ERROR` with another errno (e.g. the family is unsupported).
    Error(i32),
}

fn ne_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Decode the first message of a sock_diag reply; `None` for anything
/// truncated or unrecognised.
pub fn parse_sock_diag_reply(buf: &[u8]) -> Option<DiagReply> {
    let len = ne_u32(buf, 0)? as usize;
    let kind = u16::from_ne_bytes(buf.get(4..6)?.try_into().ok()?);
    let payload = buf.get(NLMSG_HDR_LEN..len.min(buf.len()))?;
    match kind {
        NLMSG_ERROR => {
            let errno = i32::from_ne_bytes(payload.get(0..4)?.try_into().ok()?);
            Some(if errno == -2 {
                DiagReply::NotFound
            } else {
                DiagReply::Error(errno)
            })
        }
        SOCK_DIAG_BY_FAMILY => Some(DiagReply::Found {
            state: *payload.get(MSG_STATE_OFF)?,
            uid: ne_u32(payload, MSG_UID_OFF)?,
            inode: ne_u32(payload, MSG_INODE_OFF)?,
        }),
        _ => None,
    }
}

/// The owner a decoded reply establishes, under the same rules as the `/proc`
/// arm: a TIME_WAIT or orphaned (inode 0) socket's uid is not the connector's.
pub fn owner_from_diag(reply: DiagReply) -> Result<u32, Unresolved> {
    match reply {
        DiagReply::Found { state, inode, .. } if state == DIAG_STATE_TIME_WAIT || inode == 0 => {
            Err(Unresolved::SocketClosing)
        }
        DiagReply::Found { uid, .. } => Ok(uid),
        DiagReply::NotFound => Err(Unresolved::NoMatchingSocket),
        DiagReply::Error(_) => Err(Unresolved::TableUnreadable),
    }
}
