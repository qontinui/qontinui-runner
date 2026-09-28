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
        // A zero Duration is an error to std; the caller meant "now".
        let t = t.map(|d| d.max(Duration::from_millis(1)));
        self.stream.set_read_timeout(t)?;
        self.stream.set_write_timeout(t)
    }

    /// The uid of the process at the other end, as the kernel recorded it at
    /// connect time.
    pub fn peer_uid(&self) -> io::Result<u32> {
        peer_uid(self.stream.as_raw_fd())
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

    pub fn accept(&self) -> io::Result<Conn> {
        let (stream, _) = self.inner.accept()?;
        Ok(Conn { stream })
    }

    pub fn path(&self) -> &Path {
        &self.path
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
