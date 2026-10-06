//! Named-pipe transport. See the parent module for the contract.
//!
//! Every handle is opened OVERLAPPED so that a read, a write and a connect can
//! each be bounded by a timeout (a synchronous `ReadFile` on a pipe has none,
//! and a wedged peer would pin the caller forever). Each operation waits on its
//! own event; a timeout cancels the I/O with `CancelIoEx` and then waits for
//! the cancellation to complete before the `OVERLAPPED` leaves scope.
//!
//! UNRUN on merytshost (a Linux box): this module is type-checked for
//! `x86_64-pc-windows-msvc`, never executed there.

use std::io::{self, Read, Write};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, LocalFree, DUPLICATE_SAME_ACCESS,
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::remaining;

const PIPE_BUFFER: u32 = 64 * 1024;

/// An owned kernel handle, closed on drop.
#[derive(Debug)]
struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide index, valid from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: we own this handle and close it exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

fn new_event() -> io::Result<Handle> {
    // SAFETY: manual-reset, initially non-signaled, unnamed event.
    let h = unsafe { CreateEventW(null(), 1, 0, null()) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}

fn timeout_ms(t: Option<Duration>) -> u32 {
    match t {
        None => INFINITE,
        Some(d) => d.as_millis().clamp(1, u128::from(INFINITE - 1)) as u32,
    }
}

/// A connected pipe end (either side).
#[derive(Debug)]
pub struct Conn {
    handle: Handle,
    read_event: Handle,
    write_event: Handle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    /// The holder's end of the pipe (from `Listener::accept`), as opposed to
    /// the runner's (from `connect`). Decides what [`Conn::shutdown`] can do.
    server: bool,
}

impl Conn {
    fn from_handle(handle: Handle, server: bool) -> io::Result<Conn> {
        Ok(Conn {
            handle,
            read_event: new_event()?,
            write_event: new_event()?,
            read_timeout: None,
            write_timeout: None,
            server,
        })
    }

    /// Bound every subsequent read and write; `None` blocks indefinitely.
    pub fn set_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.read_timeout = t;
        self.write_timeout = t;
        Ok(())
    }

    /// Bound every subsequent READ only. Per handle on Windows (unlike the
    /// per-socket Unix option), but kept split for the same contract.
    pub fn set_read_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.read_timeout = t;
        Ok(())
    }

    /// Bound every subsequent WRITE only.
    pub fn set_write_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.write_timeout = t;
        Ok(())
    }

    /// A second handle on the same pipe instance (`DuplicateHandle`), with its
    /// own events, so one thread can read while another writes. Each
    /// overlapped operation carries its own `OVERLAPPED` and event, so the two
    /// never complete each other's I/O.
    pub fn try_clone(&self) -> io::Result<Conn> {
        let mut dup: HANDLE = null_mut();
        // SAFETY: duplicating a handle we own into this same process; `dup` is
        // a valid out-parameter and is owned by the `Handle` below.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle.0,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Conn::from_handle(Handle(dup), self.server)
    }

    /// Block until the peer has read everything written so far
    /// (`FlushFileBuffers`). A pipe server that exits right after its last
    /// write can otherwise lose the unread tail; the holder calls this after
    /// the `exit` frame and before it exits. UNBOUNDED by itself — a peer that
    /// never reads pins the caller — so the holder only ever calls it from a
    /// connection's own pump thread, never from a thread anything waits on
    /// without a deadline.
    pub fn flush_to_peer(&self) -> io::Result<()> {
        // SAFETY: a valid pipe handle.
        if unsafe { FlushFileBuffers(self.handle.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The pid of the process that created the pipe instance we are connected
    /// to. The client compares it with the lock record, so a squatter that won
    /// the pipe name cannot pose as the pane's holder.
    pub fn server_pid(&self) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: valid pipe handle, valid out-parameter.
        if unsafe { GetNamedPipeServerProcessId(self.handle.0, &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pid)
    }

    /// End the connection now, for EVERY handle on it: cancel this handle's
    /// pending I/O and, on the holder's side, disconnect the pipe instance so
    /// a read blocked on a [`Conn::try_clone`] of it fails instead of waiting
    /// for a client that has stopped talking. (Dropping the last handle does
    /// the rest.)
    pub fn shutdown(&self) {
        // SAFETY: a valid handle; a null OVERLAPPED cancels all of its I/O.
        unsafe { CancelIoEx(self.handle.0, null()) };
        if self.server {
            // SAFETY: a valid server-side pipe handle.
            unsafe { DisconnectNamedPipe(self.handle.0) };
        }
    }

    /// Run one overlapped operation to completion or timeout.
    ///
    /// `is_read` decides how a broken pipe reads: EOF (`Ok(0)`) for a read,
    /// `BrokenPipe` for a write.
    fn overlapped(
        &self,
        event: &Handle,
        is_read: bool,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        // SAFETY: OVERLAPPED is plain data; zeroed is its initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        let ok = start(&mut ov);
        if ok == 0 {
            let e = last_error();
            if e != ERROR_IO_PENDING {
                return self.map_err(e, is_read);
            }
        }
        // SAFETY: a valid event handle.
        let timeout = if is_read {
            self.read_timeout
        } else {
            self.write_timeout
        };
        let waited = unsafe { WaitForSingleObject(event.0, timeout_ms(timeout)) };
        let mut n = 0u32;
        if waited == WAIT_TIMEOUT {
            // SAFETY: cancel exactly this operation, then WAIT for it to finish
            // so `ov` is not freed while the kernel still references it.
            unsafe { CancelIoEx(self.handle.0, &ov) };
            // SAFETY: as above; bWait = TRUE.
            let completed = unsafe { GetOverlappedResult(self.handle.0, &ov, &mut n, 1) } != 0;
            // Completed before the cancel landed, OR aborted
            // (ERROR_OPERATION_ABORTED) after moving some bytes: either way
            // those bytes were transferred and must be reported, or the
            // stream position the caller tracks is wrong. Only a transfer of
            // zero bytes is a timeout.
            if completed || n > 0 {
                return Ok(n as usize);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pipe I/O timed out",
            ));
        }
        if waited != WAIT_OBJECT_0 {
            // WAIT_FAILED: capture its error BEFORE the cleanup calls below
            // overwrite the thread's last-error value.
            let err = io::Error::last_os_error();
            // Still make sure the I/O is gone before `ov` leaves scope.
            // SAFETY: as above.
            unsafe {
                CancelIoEx(self.handle.0, &ov);
                GetOverlappedResult(self.handle.0, &ov, &mut n, 1);
            }
            return Err(err);
        }
        // SAFETY: the operation has completed; bWait = FALSE.
        if unsafe { GetOverlappedResult(self.handle.0, &ov, &mut n, 0) } == 0 {
            return self.map_err(last_error(), is_read);
        }
        Ok(n as usize)
    }

    fn map_err(&self, code: u32, is_read: bool) -> io::Result<usize> {
        let broken =
            code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA || code == ERROR_PIPE_NOT_CONNECTED;
        if broken && is_read {
            Ok(0)
        } else if broken {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let h = self.handle.0;
        let ptr = buf.as_mut_ptr();
        self.overlapped(&self.read_event, true, |ov| {
            // SAFETY: `buf` outlives the operation — `overlapped` does not
            // return until it has completed or been cancelled and drained.
            unsafe { ReadFile(h, ptr, len, null_mut(), ov) }
        })
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let h = self.handle.0;
        let ptr = buf.as_ptr();
        self.overlapped(&self.write_event, false, |ov| {
            // SAFETY: as for read.
            unsafe { WriteFile(h, ptr, len, null_mut(), ov) }
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A security descriptor from SDDL, freed with `LocalFree`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: an immutable heap block after construction.
unsafe impl Send for SecurityDescriptor {}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// The current user's SID as a UTF-16 string (no terminator).
fn current_user_sid() -> io::Result<Vec<u16>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: pseudo-handle for this process; valid out-parameter.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut len = 0u32;
    // SAFETY: a size query; expected to fail with ERROR_INSUFFICIENT_BUFFER.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    if last_error() != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage so TOKEN_USER (which holds a pointer) is suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `len` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above wrote a TOKEN_USER at the start of `buf`.
    let user = unsafe { &*(buf.as_ptr().cast::<TOKEN_USER>()) };
    let mut s: *mut u16 = null_mut();
    // SAFETY: a valid SID from the token; out-parameter freed below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    // SAFETY: `s` is a NUL-terminated wide string from the call above.
    unsafe {
        let mut p = s;
        while *p != 0 {
            out.push(*p);
            p = p.add(1);
        }
        LocalFree(s.cast());
    }
    Ok(out)
}

/// `D:P(A;;GA;;;<user SID>)` — a PROTECTED DACL (no inherited ACEs) with one
/// allow ACE, generic-all, for the current user only. Everyone else, including
/// other users and SYSTEM-less network logons, has no access at all.
fn owner_only_descriptor() -> io::Result<SecurityDescriptor> {
    let mut sddl: Vec<u16> = "D:P(A;;GA;;;".encode_utf16().collect();
    sddl.extend(current_user_sid()?);
    sddl.extend(")".encode_utf16());
    sddl.push(0);
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: NUL-terminated SDDL; out-parameter freed by SecurityDescriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(SecurityDescriptor(sd))
}

/// A listening pipe name.
///
/// **Invariant: from `bind` on, this listener always owns at least one
/// instance of the pipe name** — the `pending` one, waiting for a client. That
/// is what keeps the name ours: while an instance with our owner-only DACL
/// exists, nobody else can create another instance of that name, and once the
/// last instance closes the name is free for anyone to create with a
/// permissive DACL, which the runner would then connect to. So a replacement
/// instance is always created BEFORE the one it replaces is given away or
/// dropped, and a failed connect disconnects and REUSES its instance rather
/// than dropping it.
///
/// If the invariant were ever broken (`pending` empty), the name is
/// re-created as a FIRST instance, and `ERROR_ACCESS_DENIED` there means an
/// instance of the name still exists that is not ours to serve: a
/// [`super::FatalAcceptError`].
pub struct Listener {
    name: Vec<u16>,
    sd: SecurityDescriptor,
    state: std::sync::Mutex<AcceptState>,
}

/// What `accept` carries between calls.
struct AcceptState {
    /// The listening instance (the type invariant: `Some` from `bind` on).
    pending: Option<Handle>,
    /// When the pending instance first could neither be reset
    /// (`DisconnectNamedPipe`) nor replaced, in an unbroken run of such
    /// accepts. `None` while accepts are healthy.
    stuck_since: Option<Instant>,
    /// Consecutive "cannot reset, cannot replace" accepts, for the message.
    stuck: u32,
}

/// How long an unbroken run of "cannot reset, cannot replace" accepts may last
/// before the listener reports a [`super::FatalAcceptError`].
///
/// TIME-BASED AND LONG on purpose (plan Phase 1 hand-off, Phase 2): since
/// Phase 2 the holder owns a PTY, and an endpoint problem must never end the
/// session it holds. Phase 1's 30-consecutive-attempts bound (~25 s) would have
/// done exactly that. And even a "fatal" accept error no longer exits the
/// holder — `server::Holder::run` logs it and keeps retrying at its longest
/// backoff — so this bound now only decides when the log says "fatal".
pub const STUCK_INSTANCE_FATAL_AFTER: Duration = Duration::from_secs(15 * 60);

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener").finish_non_exhaustive()
    }
}

impl Listener {
    /// Create the FIRST instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a
    /// pipe of that name that already exists — a squatter — fails the bind.
    pub fn bind(name: &str) -> io::Result<Listener> {
        let listener = Listener {
            name: wide(name),
            sd: owner_only_descriptor()?,
            state: std::sync::Mutex::new(AcceptState {
                pending: None,
                stuck_since: None,
                stuck: 0,
            }),
        };
        let first = listener.create_first_instance()?;
        listener
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending = Some(first);
        Ok(listener)
    }

    /// A first instance; `ERROR_ACCESS_DENIED` means an instance of the name
    /// already exists, which is fatal.
    ///
    /// That is not only a squatter: an open CLIENT handle to a previous holder
    /// of this pane keeps that holder's instance alive after it died, so a
    /// runner that still holds an idle `Client` produces the same error. The
    /// runner must drop its clients before respawning a pane's holder.
    fn create_first_instance(&self) -> io::Result<Handle> {
        self.create_instance(true).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                io::Error::other(super::FatalAcceptError(format!(
                    "an instance of this pipe name still exists (a squatter, or a \
                     stale client handle to a previous holder): {e}"
                )))
            } else {
                e
            }
        })
    }

    fn create_instance(&self, first: bool) -> io::Result<Handle> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: NUL-terminated name, valid security attributes that outlive
        // the call.
        let h = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Handle(h))
    }

    /// Wait for a client on `h`.
    fn connect_instance(h: &Handle) -> io::Result<()> {
        let event = new_event()?;
        // SAFETY: plain data, zeroed initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        // SAFETY: valid pipe handle; `ov` outlives the wait below.
        let ok = unsafe { ConnectNamedPipe(h.0, &mut ov) };
        if ok != 0 {
            return Ok(());
        }
        match last_error() {
            ERROR_PIPE_CONNECTED => Ok(()),
            ERROR_IO_PENDING => {
                let mut n = 0u32;
                // SAFETY: valid handles; wait for the connect to finish.
                let done = unsafe {
                    WaitForSingleObject(event.0, INFINITE);
                    GetOverlappedResult(h.0, &ov, &mut n, 1)
                };
                if done == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }

    /// Wait for a client, then hand its instance out — but only after a
    /// replacement instance exists (see the type's invariant).
    pub fn accept(&self) -> io::Result<Conn> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let h = match st.pending.take() {
            Some(h) => h,
            // Unreachable while the invariant holds; if it was broken, the
            // name may have zero instances, so only a FIRST instance is safe.
            None => self.create_first_instance()?,
        };
        match Self::connect_instance(&h) {
            Ok(()) => match self.create_instance(false) {
                Ok(next) => {
                    st.pending = Some(next);
                    st.stuck = 0;
                    st.stuck_since = None;
                    drop(st);
                    Conn::from_handle(h, true)
                }
                Err(e) => {
                    // No replacement: this client cannot be served without
                    // leaving the name with zero listening instances once it
                    // hangs up. Drop the client, keep the instance.
                    // SAFETY: valid pipe handle.
                    unsafe { DisconnectNamedPipe(h.0) };
                    st.pending = Some(h);
                    Err(e)
                }
            },
            Err(e) => {
                // A client that came and went (ERROR_NO_DATA), or a failed
                // wait. Disconnect and reuse this instance; if it cannot be
                // reset, replace it BEFORE dropping it.
                // SAFETY: valid pipe handle.
                if unsafe { DisconnectNamedPipe(h.0) } != 0 {
                    st.pending = Some(h);
                    st.stuck = 0;
                    st.stuck_since = None;
                    return Err(e);
                }
                let disconnect_err = io::Error::last_os_error();
                match self.create_instance(false) {
                    Ok(next) => {
                        st.pending = Some(next);
                        st.stuck = 0;
                        st.stuck_since = None;
                        drop(h);
                        Err(e)
                    }
                    Err(create_err) => {
                        // Neither reset nor replaced: keep the (possibly
                        // broken) instance so the name stays ours, but do not
                        // retry it forever.
                        st.pending = Some(h);
                        st.stuck += 1;
                        let since = *st.stuck_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= STUCK_INSTANCE_FATAL_AFTER {
                            return Err(io::Error::other(super::FatalAcceptError(format!(
                                "pipe instance unusable for {:?} ({} consecutive attempts): \
                                 connect failed ({e}), DisconnectNamedPipe failed \
                                 ({disconnect_err}), CreateNamedPipeW failed ({create_err})",
                                since.elapsed(),
                                st.stuck
                            ))));
                        }
                        Err(e)
                    }
                }
            }
        }
    }
}

/// Connect to the pipe, waiting for a free instance until `deadline`.
///
/// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` caps what the server can
/// do with our token at "identify", so a squatting server cannot impersonate
/// the runner.
pub fn connect(name: &str, deadline: Instant) -> io::Result<Conn> {
    let wname = wide(name);
    loop {
        // SAFETY: NUL-terminated name; no security attributes; no template.
        let h = unsafe {
            CreateFileW(
                wname.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            return Conn::from_handle(Handle(h), false);
        }
        match last_error() {
            ERROR_PIPE_BUSY => {
                let left = remaining(deadline)?;
                // SAFETY: NUL-terminated name.
                unsafe { WaitNamedPipeW(wname.as_ptr(), timeout_ms(Some(left))) };
            }
            ERROR_FILE_NOT_FOUND => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no pipe endpoint for this pane",
                ))
            }
            code => return Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}
