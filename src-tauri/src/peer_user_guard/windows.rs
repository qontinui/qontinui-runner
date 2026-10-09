//! Windows owner resolver: `GetExtendedTcpTable` → owning PID → process
//! token → user SID.
//!
//! The comparison is on the token USER SID, so an elevated and a
//! non-elevated process of the same account WOULD compare equal — but only
//! when the token can be read, and a non-elevated runner is expected to be
//! refused `OpenProcessToken` on an elevated caller's token (its DACL grants
//! Administrators, not the user). That case resolves `TokenAccessDenied`,
//! counted on its own so Windows' default `shadow` mode measures it before
//! enforcing (plan Phase 2 check 2). A process this runner cannot open, or
//! whose token it cannot read, is refused under `enforce` (fail closed) —
//! which is the outcome for another account's processes on a standard
//! (non-admin) runner.
//!
//! PID reuse: a row's owning PID is only trusted when that process was
//! created no later than the accept instant. A process born after the
//! connection existed cannot own it.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, FILETIME,
    HANDLE, NO_ERROR,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
    TCP_TABLE_OWNER_PID_CONNECTIONS,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::decode::{filetime_to_system_time, owner_pid, win_ipv4, win_port, WinRow};
use super::{LocalUser, OwnerResolver, Resolution, Unresolved};

const AF_INET: u32 = 2;
const CLOCK_SLACK: std::time::Duration = std::time::Duration::from_secs(2);
const AF_INET6: u32 = 23;

pub struct TcpTableResolver;

/// Closes a HANDLE on drop.
struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned open by a Win32 call and is
            // closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Last observed table size per family, so a steady stream of connections
/// starts at the right buffer size instead of paying a size probe each time.
static V4_SIZE_HINT: AtomicU32 = AtomicU32::new(0);
static V6_SIZE_HINT: AtomicU32 = AtomicU32::new(0);

/// Read one family's owner-PID connection table into raw bytes.
fn read_table(family: u32) -> Result<Vec<u64>, Unresolved> {
    let hint = if family == AF_INET {
        &V4_SIZE_HINT
    } else {
        &V6_SIZE_HINT
    };
    let mut size: u32 = hint.load(Ordering::Relaxed);
    for _ in 0..4 {
        // u64 backing store keeps the table 8-byte aligned.
        let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8).max(1)];
        let mut len = (buf.len() * 8) as u32;
        // SAFETY: `buf` is writable for `len` bytes; the call writes at most
        // `len` bytes and reports the size it needs otherwise.
        let rc = unsafe {
            GetExtendedTcpTable(
                buf.as_mut_ptr().cast(),
                &mut len,
                0,
                family,
                TCP_TABLE_OWNER_PID_CONNECTIONS,
                0,
            )
        };
        if rc == NO_ERROR {
            hint.store(len.saturating_add(4096), Ordering::Relaxed);
            return Ok(buf);
        }
        if rc != ERROR_INSUFFICIENT_BUFFER {
            return Err(Unresolved::TableUnreadable);
        }
        // Connections come and go between the size probe and the read; pad.
        size = len + 4096;
    }
    Err(Unresolved::TableUnreadable)
}

fn rows_v4() -> Result<Vec<WinRow>, Unresolved> {
    let buf = read_table(AF_INET)?;
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: a successful call wrote a MIB_TCPTABLE_OWNER_PID: a u32 count
    // followed by `count` rows, all inside `buf`.
    let count = unsafe { base.cast::<u32>().read_unaligned() } as usize;
    let first = unsafe { base.add(4) }.cast::<MIB_TCPROW_OWNER_PID>();
    let max = (buf.len() * 8).saturating_sub(4) / std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
    Ok((0..count.min(max))
        .map(|i| {
            // SAFETY: `i < count` and the row lies inside `buf` (bounded by `max`).
            let r = unsafe { first.add(i).read_unaligned() };
            WinRow {
                local: SocketAddr::new(
                    IpAddr::V4(win_ipv4(r.dwLocalAddr)),
                    win_port(r.dwLocalPort),
                ),
                remote: SocketAddr::new(
                    IpAddr::V4(win_ipv4(r.dwRemoteAddr)),
                    win_port(r.dwRemotePort),
                ),
                state: r.dwState,
                pid: r.dwOwningPid,
            }
        })
        .collect())
}

fn rows_v6() -> Result<Vec<WinRow>, Unresolved> {
    let buf = read_table(AF_INET6)?;
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: as in rows_v4, for MIB_TCP6TABLE_OWNER_PID.
    let count = unsafe { base.cast::<u32>().read_unaligned() } as usize;
    let first = unsafe { base.add(4) }.cast::<MIB_TCP6ROW_OWNER_PID>();
    let max = (buf.len() * 8).saturating_sub(4) / std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>();
    Ok((0..count.min(max))
        .map(|i| {
            // SAFETY: bounded by `count` and `max`.
            let r = unsafe { first.add(i).read_unaligned() };
            WinRow {
                local: SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
                    win_port(r.dwLocalPort),
                ),
                remote: SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::from(r.ucRemoteAddr)),
                    win_port(r.dwRemotePort),
                ),
                state: r.dwState,
                pid: r.dwOwningPid,
            }
        })
        .collect())
}

/// The user SID of `process`'s token, in canonical string form.
fn token_user_sid(process: HANDLE) -> Result<String, Unresolved> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a valid handle with query rights; `token` receives
    // a new handle we own.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        // SAFETY: no preconditions; read immediately after the failing call.
        return Err(if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            Unresolved::TokenAccessDenied
        } else {
            Unresolved::TokenUnreadable
        });
    }
    let token = Owned(token);
    let mut needed: u32 = 0;
    // SAFETY: size probe with a null buffer.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(Unresolved::TokenUnreadable);
    }
    let mut buf: Vec<u64> = vec![0; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` is writable for at least `needed` bytes and 8-aligned.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(Unresolved::TokenUnreadable);
    }
    // SAFETY: the call wrote a TOKEN_USER at the start of `buf`.
    let user = unsafe { buf.as_ptr().cast::<TOKEN_USER>().read() };
    let mut wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: `user.User.Sid` points into `buf`, alive for this call;
    // `wide` receives a LocalAlloc'd string we free below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut wide) } == 0 || wide.is_null() {
        return Err(Unresolved::TokenUnreadable);
    }
    // SAFETY: `wide` is a NUL-terminated UTF-16 string.
    let s = unsafe {
        let mut n = 0;
        while *wide.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(wide, n))
    };
    // SAFETY: freeing the LocalAlloc'd buffer exactly once.
    unsafe { LocalFree(wide.cast()) };
    Ok(s)
}

fn creation_time(process: HANDLE) -> Option<SystemTime> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
    // SAFETY: valid handle with PROCESS_QUERY_LIMITED_INFORMATION.
    if unsafe { GetProcessTimes(process, &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    filetime_to_system_time(c.dwLowDateTime, c.dwHighDateTime)
}

impl OwnerResolver for TcpTableResolver {
    fn own_user(&self) -> Result<LocalUser, String> {
        // SAFETY: the pseudo-handle needs no close.
        token_user_sid(unsafe { GetCurrentProcess() })
            .map(LocalUser::Sid)
            .map_err(|e| format!("own process token: {}", e.as_str()))
    }

    fn resolve(&self, local: SocketAddr, peer: SocketAddr, accepted_at: SystemTime) -> Resolution {
        let pid = match rows_v4().and_then(|r| owner_pid(r, local, peer)) {
            Ok(pid) => pid,
            Err(Unresolved::NoMatchingSocket) => {
                match rows_v6().and_then(|r| owner_pid(r, local, peer)) {
                    Ok(pid) => pid,
                    Err(e) => {
                        return Resolution {
                            owner: Err(e),
                            pid: None,
                        }
                    }
                }
            }
            Err(e) => {
                return Resolution {
                    owner: Err(e),
                    pid: None,
                }
            }
        };
        // SAFETY: no preconditions.
        if pid == unsafe { GetCurrentProcessId() } {
            return Resolution {
                owner: self.own_user().map_err(|_| Unresolved::TokenUnreadable),
                pid: Some(pid),
            };
        }
        // SAFETY: OpenProcess returns null on failure; a non-null handle is
        // ours to close.
        let process = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
        if process.0.is_null() {
            return Resolution {
                owner: Err(Unresolved::ProcessUnopenable),
                pid: Some(pid),
            };
        }
        match creation_time(process.0) {
            // A couple of seconds of slack absorbs a small backwards wall-clock
            // step between the two readings. What this catches: a PID reused
            // AFTER the accept. Residual (plan residuals): a socket inherited by
            // a child after its creator exits names a dead PID, and reuse of
            // that PID BEFORE the accept is not detected — an attacker cannot
            // choose which process receives a reused PID.
            Some(created) if created <= accepted_at + CLOCK_SLACK => {}
            // Born after the connection existed, or unreadable: not provably
            // the owner.
            _ => {
                return Resolution {
                    owner: Err(Unresolved::PidRecycled),
                    pid: Some(pid),
                }
            }
        }
        Resolution {
            owner: token_user_sid(process.0).map(LocalUser::Sid),
            pid: Some(pid),
        }
    }
}
