//! Unix-domain socket transport. See the parent module for the contract.

use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::remaining;

/// The effective uid of this process.
pub fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// A connected stream (either side).
#[derive(Debug)]
pub struct Conn {
    stream: UnixStream,
}

impl Conn {
    /// Bound every subsequent read and write; `None` blocks indefinitely.
    pub fn set_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(t)?;
        self.set_write_timeout(t)
    }

    /// Bound every subsequent READ only.
    ///
    /// The socket option is per SOCKET, so it is shared with every
    /// [`Conn::try_clone`] of this connection. The data path therefore keeps a
    /// strict split: the connection's reading thread is the only one that sets
    /// read timeouts and its writer the only one that sets write timeouts — a
    /// combined [`Conn::set_timeout`] on either half would re-arm the other's.
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        // A zero Duration is an error to std; the caller meant "now".
        self.stream
            .set_read_timeout(t.map(|d| d.max(Duration::from_millis(1))))
    }

    /// Bound every subsequent WRITE only. See [`Conn::set_read_timeout`].
    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.stream
            .set_write_timeout(t.map(|d| d.max(Duration::from_millis(1))))
    }

    /// A second handle on the same connection, so one thread can read while
    /// another writes (the attached data path: the holder's per-connection
    /// output pump writes while the dispatch loop reads).
    pub fn try_clone(&self) -> io::Result<Conn> {
        Ok(Conn {
            stream: self.stream.try_clone()?,
        })
    }

    /// Nothing to drain on a Unix socket: bytes written before `close` are
    /// still delivered to the peer. See the Windows twin for why it exists.
    pub fn flush_to_peer(&self) -> io::Result<()> {
        Ok(())
    }

    /// The uid of the process at the other end, as the kernel recorded it at
    /// connect time.
    pub fn peer_uid(&self) -> io::Result<u32> {
        peer_uid(self.stream.as_raw_fd())
    }

    /// Wrap one end of a socketpair, so server tests can drive
    /// `serve_conn_with` in-process.
    #[cfg(test)]
    pub(crate) fn from_stream(stream: UnixStream) -> Conn {
        Conn { stream }
    }

    /// Stop READING this connection. **Linux** semantics, on which a holder
    /// upgrade's input drain relies: data already queued stays readable,
    /// then reads return EOF, and the peer's further writes fail (`EPIPE`).
    /// BSD-family kernels (macOS included) may instead discard the queued
    /// data and accept later writes silently, so this is Linux-only and the
    /// drain elsewhere never shuts a connection down (`Pane::drain_input_for_handoff`).
    #[cfg(target_os = "linux")]
    pub fn shutdown_read(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Read);
    }

    /// Half-close nothing, close everything: used after a rejection.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn timed_out(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::WouldBlock {
        io::Error::new(io::ErrorKind::TimedOut, e)
    } else {
        e
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf).map_err(timed_out)
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf).map_err(timed_out)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A listening endpoint.
#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
}

impl Listener {
    /// Bind at `path` and tighten it to 0600. The pane directory's 0700 is the
    /// real barrier (nobody else can reach the path at all); the socket mode is
    /// defence in depth for the moment between `bind` and `chmod`.
    pub fn bind(path: &Path) -> io::Result<Listener> {
        check_path_len(path)?;
        let inner = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            inner,
            path: path.to_path_buf(),
        })
    }

    /// Bind a holder upgrade's successor endpoint BESIDE `path`, at
    /// [`staged_path`] — before the successor acks, so a bind failure is a
    /// `nak` (the predecessor keeps the pane) rather than a failure after
    /// the commit. [`StagedListener::publish`] then moves it over `path` with
    /// one `rename`.
    pub fn bind_staged(path: &Path) -> io::Result<StagedListener> {
        let tmp = staged_path(path);
        check_path_len(&tmp)?;
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let inner = UnixListener::bind(&tmp)?;
        let staged = StagedListener {
            listener: Some(Listener {
                inner,
                path: path.to_path_buf(),
            }),
            tmp,
        };
        std::fs::set_permissions(&staged.tmp, std::fs::Permissions::from_mode(0o600))?;
        Ok(staged)
    }

    pub fn accept(&self) -> io::Result<Conn> {
        let (stream, _) = self.inner.accept()?;
        Ok(Conn { stream })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Where [`Listener::bind_staged`] binds: `<path>.upg`.
pub fn staged_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".upg");
    PathBuf::from(tmp)
}

/// A successor's listener, bound but not yet at the pane's endpoint path.
/// Dropped unpublished, it removes its staged socket file.
#[derive(Debug)]
pub struct StagedListener {
    listener: Option<Listener>,
    tmp: PathBuf,
}

impl StagedListener {
    /// `rename` the staged socket over the endpoint path: one atomic step, so
    /// a client connecting by path reaches the predecessor's socket or this
    /// one. (The predecessor's socket may already be dead by then — its
    /// holder exits once the successor reports `serving`, or after a bound;
    /// a client in that window gets a refusal and retries.) A failed rename
    /// is retried briefly; if it still fails the listener is returned anyway
    /// with the error: the pane keeps running UNREACHABLE (its lock held, so
    /// a census reads it UNKNOWN), never ended for an I/O error.
    pub fn publish(mut self) -> (Listener, io::Result<()>) {
        let listener = self.listener.take().expect("published once");
        let mut result = Ok(());
        for attempt in 0..5 {
            result = std::fs::rename(&self.tmp, &listener.path);
            if result.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20 << attempt));
        }
        if result.is_err() {
            let _ = std::fs::remove_file(&self.tmp);
        }
        (listener, result)
    }
}

impl Drop for StagedListener {
    fn drop(&mut self) {
        if self.listener.is_some() {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// `sun_path` capacity minus the terminating NUL (107 on Linux, 103 on macOS).
fn max_path_len() -> usize {
    // SAFETY: sockaddr_un is plain data; zeroed is a valid value.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_path.len() - 1
}

fn check_path_len(path: &Path) -> io::Result<()> {
    let n = path.as_os_str().as_bytes().len();
    if n > max_path_len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path {} is {n} bytes; the OS limit is {}",
                path.display(),
                max_path_len()
            ),
        ));
    }
    Ok(())
}

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn new_socket() -> io::Result<OwnedFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: plain syscall; the returned fd is owned immediately below.
        let fd = cvt(unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        })?;
        // SAFETY: `fd` was just returned by socket() and is owned by nobody else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        // SAFETY: as above.
        let fd = cvt(unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) })?;
        // SAFETY: as above.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fcntl on a valid, owned fd.
        unsafe {
            cvt(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC))?;
            let fl = cvt(libc::fcntl(fd, libc::F_GETFL))?;
            cvt(libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK))?;
        }
        Ok(owned)
    }
}

/// Connect without ever blocking past `deadline`.
///
/// A plain blocking `connect` to a listener whose backlog is full BLOCKS on
/// Linux — exactly the wedged-holder case a probe must survive — so the socket
/// is non-blocking for the connect and put back to blocking after it.
pub fn connect(path: &Path, deadline: Instant) -> io::Result<Conn> {
    check_path_len(path)?;
    let fd = new_socket()?;
    let raw = fd.as_raw_fd();

    // SAFETY: sockaddr_un is plain data; zeroed is valid.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    let base = std::mem::size_of::<libc::sockaddr_un>() - addr.sun_path.len();
    let len = (base + bytes.len() + 1) as libc::socklen_t;

    loop {
        // SAFETY: `addr` is a fully initialized sockaddr_un and `len` is within it.
        let rc =
            unsafe { libc::connect(raw, std::ptr::addr_of!(addr).cast::<libc::sockaddr>(), len) };
        if rc == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            // Linux: the listener's backlog is full. Retry until the deadline.
            Some(libc::EAGAIN) => {
                let left = remaining(deadline)?;
                std::thread::sleep(left.min(Duration::from_millis(5)));
            }
            Some(libc::EINPROGRESS) => {
                wait_writable(raw, deadline)?;
                let mut so_err: libc::c_int = 0;
                let mut sl = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                // SAFETY: valid fd, correctly sized out-parameters.
                cvt(unsafe {
                    libc::getsockopt(
                        raw,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        std::ptr::addr_of_mut!(so_err).cast(),
                        &mut sl,
                    )
                })?;
                if so_err != 0 {
                    return Err(io::Error::from_raw_os_error(so_err));
                }
                break;
            }
            _ => return Err(err),
        }
    }

    // Back to blocking; reads and writes are bounded by timeouts instead.
    // SAFETY: fcntl on a valid, owned fd.
    unsafe {
        let fl = cvt(libc::fcntl(raw, libc::F_GETFL))?;
        cvt(libc::fcntl(raw, libc::F_SETFL, fl & !libc::O_NONBLOCK))?;
    }
    Ok(Conn {
        stream: UnixStream::from(fd),
    })
}

fn wait_writable(fd: libc::c_int, deadline: Instant) -> io::Result<()> {
    loop {
        let left = remaining(deadline)?;
        let ms = left.as_millis().clamp(1, libc::c_int::MAX as u128) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc > 0 {
            return Ok(());
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINTR) {
                return Err(err);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(fd: libc::c_int) -> io::Result<u32> {
    // SAFETY: ucred is plain data.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd, correctly sized out-parameters.
    cvt(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    })?;
    Ok(cred.uid)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_uid(fd: libc::c_int) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: valid fd and out-parameters.
    cvt(unsafe { libc::getpeereid(fd, &mut uid, &mut gid) })?;
    Ok(uid)
}

/// Any other Unix: no known peer-credential call, so FAIL CLOSED — every peer
/// is rejected rather than every peer admitted.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn peer_uid(_fd: libc::c_int) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no peer-credential call on this OS; refusing every peer",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_unix_peer_uid_of_a_socketpair_is_ours() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(a.as_raw_fd()).unwrap(), own_uid());
    }

    #[test]
    fn pty_holder_unix_overlong_socket_path_is_refused() {
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(200)));
        let err = connect(&long, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
