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
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, WaitNamedPipeW,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
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
    timeout: Option<Duration>,
}

impl Conn {
    fn from_handle(handle: Handle) -> io::Result<Conn> {
        Ok(Conn {
            handle,
            read_event: new_event()?,
            write_event: new_event()?,
            timeout: None,
        })
    }

    /// Bound every subsequent read and write; `None` blocks indefinitely.
    pub fn set_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        self.timeout = t;
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

    /// Close the connection after a rejection (drop does the same).
    pub fn shutdown(&self) {}

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
        let waited = unsafe { WaitForSingleObject(event.0, timeout_ms(self.timeout)) };
        let mut n = 0u32;
        if waited == WAIT_TIMEOUT {
            // SAFETY: cancel exactly this operation, then WAIT for it to finish
            // so `ov` is not freed while the kernel still references it.
            unsafe { CancelIoEx(self.handle.0, &ov) };
            // SAFETY: as above; bWait = TRUE.
            if unsafe { GetOverlappedResult(self.handle.0, &ov, &mut n, 1) } != 0 {
                // It completed before the cancel landed: keep the bytes.
                return Ok(n as usize);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pipe I/O timed out",
            ));
        }
        if waited != WAIT_OBJECT_0 {
            // Not signaled and not timed out: still make sure the I/O is gone.
            // SAFETY: as above.
            unsafe {
                CancelIoEx(self.handle.0, &ov);
                GetOverlappedResult(self.handle.0, &ov, &mut n, 1);
            }
            return Err(io::Error::last_os_error());
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

/// A listening pipe name. Always has one instance waiting for a client.
pub struct Listener {
    name: Vec<u16>,
    sd: SecurityDescriptor,
    pending: std::sync::Mutex<Option<Handle>>,
}

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
            pending: std::sync::Mutex::new(None),
        };
        let first = listener.create_instance(true)?;
        *listener.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(first);
        Ok(listener)
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

    /// Wait for a client on the pending instance, then create the next one
    /// before returning so a second client never finds the name absent.
    pub fn accept(&self) -> io::Result<Conn> {
        let mut slot = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let h = match slot.take() {
            Some(h) => h,
            None => self.create_instance(false)?,
        };
        let event = new_event()?;
        // SAFETY: plain data, zeroed initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        // SAFETY: valid pipe handle; `ov` outlives the wait below.
        let ok = unsafe { ConnectNamedPipe(h.0, &mut ov) };
        if ok == 0 {
            match last_error() {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    let mut n = 0u32;
                    // SAFETY: valid handles; wait for the connect to finish.
                    let done = unsafe {
                        WaitForSingleObject(event.0, INFINITE);
                        GetOverlappedResult(h.0, &ov, &mut n, 1)
                    };
                    if done == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        // Best effort: a failure here is retried by the next accept.
        *slot = self.create_instance(false).ok();
        drop(slot);
        Conn::from_handle(h)
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
            return Conn::from_handle(Handle(h));
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
